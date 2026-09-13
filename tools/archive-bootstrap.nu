#!/usr/bin/env nu

def capture [program: string, arguments: list<string>]: nothing -> string {
  let result = (^$program ...$arguments | complete)
  if $result.exit_code != 0 { error make {msg: $result.stderr} }
  $result.stdout | str trim
}

def main [
  --state: path
  --destination: path
]: nothing -> nothing {
  if $destination == null { error make {msg: "provide an archive directory with --destination"} }
  let base = ($env.XDG_STATE_HOME? | default ($env.HOME | path join .local/state))
  let state = ($state | default ($base | path join repkgs/bootstrap-llvm) | path expand)
  let destination = ($destination | path expand)
  let graph = ($state | path join graph.json | open)
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
  let store = ($state | path join store)
  let uri = $"file://($destination)"
  let sources = (capture nix ([path-info --store $store --json --json-format "2"] ++ $source_paths) | from json)
  if $sources.version != 2 { error make {msg: "expected Nix path-info JSON version 2"} }
  if ($sources.info | values | any {|source| ($source.references | is-not-empty) }) {
    error make {msg: "fixed-output inputs reference other store paths"}
  }
  capture nix ([store verify --store $store --no-trust --quiet] ++ $source_paths) | ignore
  mkdir $destination
  print $"Archiving ($source_paths | length) fixed-output inputs"
  ^nix --store $store copy --to $uri --no-check-sigs ...$source_paths
  print $"Archiving ($derivations | length) derivation recipes"
  ^nix --store $store copy --derivation --to $uri --no-check-sigs ...$derivations
  {
    created: (date now)
    llvm_derivation: (open ($state | path join plan.json)).derivation
    fixed_outputs: $outputs
    derivations: $derivations
  } | to json | save --force ($destination | path join repkgs-bootstrap-manifest.json)
  print $"Archive complete at ($destination)"
}
