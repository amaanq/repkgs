#!/usr/bin/env nu

use ../bootstrap/store.nu [capture plan-dir save-progress store-args]

def inventory [metadata: path]: nothing -> record {
   let graph = ($metadata | path join graph.json | open)
   if $graph.version != 4 { error make {msg: "expected Nix derivation JSON version 4"} }
   let entries = ($graph.derivations | transpose path drv)
   let outputs = ($entries | each {|entry|
      $entry.drv.outputs | transpose name spec | where spec.hash? != null | each {|output|
         let path = ($output.spec.path? | default ($entry.drv.env | get -o $output.name))
         if $path == null { error make {msg: $"missing output path for ($entry.path)"} }
         {
            derivation: $entry.path, name: $output.name
            path: (if $path starts-with '/' { $path } else { $"/nix/store/($path)" })
            hash: $output.spec.hash, method: $output.spec.method
         }
      }
   } | flatten | sort-by derivation name)
   let source_paths = ($outputs | get path | sort | uniq)
   let derivations = ($entries | get path | each {|path|
      if ($path | str starts-with "/") { $path } else { $"/nix/store/($path)" }
   } | sort | uniq)
   let inputs = ($entries | get drv.inputs.srcs | flatten | each {|path| $"/nix/store/($path)" })
   {
      derivation: (open ($metadata | path join plan.json)).derivation
      fixed_outputs: $outputs
      derivations: $derivations
      paths: ($derivations ++ $inputs ++ $source_paths | sort | uniq)
   }
}

def verify-sources [directory: path, outputs: table, jobs: int]: nothing -> nothing {
   let store = ($directory | path join store)
   let source_paths = ($outputs | get path | sort | uniq)
   let sources = (capture nix ([path-info --json --json-format "2"] ++ $source_paths ++ (store-args $directory)) | from json)
   if $sources.version != 2 { error make {msg: "expected Nix path-info JSON version 2"} }
   if ($sources.info | values | any {|source| ($source.references | is-not-empty) }) {
      error make {msg: "fixed-output inputs reference other store paths"}
   }
   print $"Verifying ($source_paths | length) fixed-output hashes with ($jobs) workers"
   $outputs | uniq-by path hash method | par-each --threads $jobs {|source|
      let command = (match $source.method {
         flat => "file"
         nar => "path"
         _ => { error make {msg: $"unsupported fixed-output hash method ($source.method)"} }
      })
      let physical = ($store | path join ($source.path | str trim --left --char '/'))
      if (capture nix [hash $command $physical]) != $source.hash {
         error make {msg: $"fixed-output input differs from its pinned hash ($source.path)"}
      }
   } | ignore
}

def main [
   --state: path
   --destination: path
   --jobs: int = 30
]: nothing -> nothing {
   if $destination == null { error make {msg: "provide an archive directory with --destination"} }
   if $jobs < 1 { error make {msg: "jobs must be positive"} }
   let base = ($env.XDG_STATE_HOME? | default ($env.HOME | path join .local/state))
   let state = ($state | default ($base | path join motley/bootstrap-llvm) | path expand)
   let destination = ($destination | path expand)
   let metadata = (plan-dir $state)
   let catalog = (inventory $metadata)
   let source_paths = ($catalog.fixed_outputs | get path | sort | uniq)
   let store = ($state | path join store)
   let uri = $"file://($destination)?compression=zstd&compression-level=6&parallel-compression=false"
   let saved = ($destination | path join metadata)
   if ($destination | path exists) and (ls --all $destination | is-not-empty) {
      if not ($saved | path join plan.json | path exists) {
         error make {msg: "archive directory is not empty and has no saved bootstrap plan"}
      }
      if (inventory (plan-dir $saved)) != $catalog {
         error make {msg: "archive belongs to a different bootstrap plan, choose another destination"}
      }
   }
   verify-sources $state $catalog.fixed_outputs $jobs
   mkdir $destination
   for directory in ([$state $metadata] | uniq) {
      let target = (if $directory == $state { $saved } else { $saved | path join peanut })
      mkdir $target
      for name in [plan.json graph.json audit.json] { cp --force ($directory | path join $name) ($target | path join $name) }
   }
   let comparisons = ($metadata | path join checkpoints.json)
   if ($comparisons | path exists) { cp --force $comparisons ($saved | path join baseline-checkpoints.json) }
   print $"Archiving ($source_paths | length) fixed-output inputs"
   ^nix --store $store copy --to $uri --no-check-sigs --option substitute false ...$source_paths
   print $"Archiving ($catalog.derivations | length) derivation recipes and their source files"
   ^nix --store $store copy --derivation --to $uri --no-check-sigs --option substitute false $catalog.derivation
   let archived = (capture nix ([path-info --store $uri --json --json-format "2"] ++ $catalog.paths) | from json)
   $catalog | insert created (date now) | insert nar_bytes ($archived.info | values | get narSize | math sum)
      | to json | save --force ($destination | path join motley-bootstrap-manifest.tmp.json)
   mv --force ($destination | path join motley-bootstrap-manifest.tmp.json) ($destination | path join motley-bootstrap-manifest.json)
   print $"Archive complete at ($destination)"
}

def "main restore" [archive: path, --state: path, --jobs: int = 30]: nothing -> nothing {
   if $state == null { error make {msg: "provide a new bootstrap directory with --state"} }
   if $jobs < 1 { error make {msg: "jobs must be positive"} }
   let directory = ($state | path expand)
   let source = ($archive | path expand --strict)
   let metadata = ($source | path join metadata)
   let catalog = (inventory (plan-dir $metadata))
   let manifest = (open ($source | path join motley-bootstrap-manifest.json))
   if ($manifest | select derivation fixed_outputs derivations paths) != $catalog {
      error make {msg: "archive manifest differs from its saved bootstrap graph"}
   }
   let store = ($directory | path join store)
   let uri = $"file://($source)"
   if ($directory | path exists) and (ls --all $directory | is-not-empty) {
      let progress = ($directory | path join progress.json)
      if not ($progress | path exists) {
         error make {msg: "restore requires an empty state directory or an interrupted restore"}
      }
      let previous = (open $progress)
      if $previous.phase != restoring or $previous.archive? != $source or $previous.target? != $catalog.derivation {
         error make {msg: "state directory does not contain an interrupted restore of this archive"}
      }
      if ($store | path exists) {
         let existing = (capture nix ([path-info --all] ++ (store-args $directory)) | lines)
         if ($existing | any {|path| $path not-in $catalog.paths }) {
            error make {msg: "interrupted restore contains paths outside the archive input list"}
         }
      }
   }
   mkdir $directory
   save-progress $directory {phase: restoring, archive: $source, target: $catalog.derivation}
   print $"Restoring ($catalog.paths | length) source and recipe paths into ($store)"
   ^nix copy --from $uri --no-check-sigs ...$catalog.paths ...(store-args $directory)
   let restored = (capture nix ([path-info --all] ++ (store-args $directory)) | lines | sort)
   if $restored != $catalog.paths { error make {msg: "restored store differs from the archive input list"} }
   let graph = (capture nix ([derivation show --recursive $catalog.derivation] ++ (store-args $directory)) | from json)
   if $graph != (open (plan-dir $metadata | path join graph.json)) {
      error make {msg: "restored derivations differ from the saved bootstrap graph"}
   }
   verify-sources $directory $catalog.fixed_outputs $jobs
   let active = (plan-dir $metadata)
   for origin in ([$metadata $active] | uniq) {
      let target = (if $origin == $metadata { $directory } else { $directory | path join peanut })
      mkdir $target
      for name in [plan.json graph.json audit.json] { cp --force ($origin | path join $name) ($target | path join $name) }
      {paths: $restored, imported_sources: $catalog.fixed_outputs, prepared: (date now)} | to json
         | save --force ($target | path join prepared.json)
   }
   mkdir ($directory | path join build)
   save-progress $directory {phase: ready, archive: $source, target: $catalog.derivation, restored_paths: ($restored | length)}
   print $"Restore verified, bootstrap state is ready at ($directory)"
}
