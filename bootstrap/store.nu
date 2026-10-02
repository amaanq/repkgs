use ../builder/glob.nu files

const bootstrap_dir = path self .

export def capture [program: string, arguments: list<string>]: nothing -> string {
  let result = (^$program ...$arguments | complete)
  if $result.exit_code != 0 {
    error make {msg: $"($program) exited with code ($result.exit_code)\n($result.stderr)"}
  }
  $result.stdout | str trim
}

export def save-progress [directory: string, progress: record]: nothing -> nothing {
  $progress | merge {updated: (date now)} | to json
    | save --force ($directory | path join progress.tmp.json)
  mv --force ($directory | path join progress.tmp.json) ($directory | path join progress.json)
}

export def store-args [directory: string, --shell: string = "", --build-dir: string = ""]: nothing -> list<string> {
  let paths = (if $shell == "" { "" } else { $"/bin/sh=($shell)" })
  let scratch = (if $build_dir == "" { $directory | path join build } else { $build_dir })
  [--store ($directory | path join store)
    --option substitute "false" --option substituters "" --option builders ""
    --option connect-timeout "15" --option stalled-download-timeout "30"
    --option sandbox "true" --option sandbox-fallback "false"
    --option sandbox-paths $paths --option extra-sandbox-paths ""
    --option build-users-group "" --option auto-allocate-uids "false" --option use-cgroups "false"
    --option keep-derivations "true" --option keep-outputs "true"
    --option build-dir $scratch
    --extra-experimental-features "nix-command ca-derivations"]
}

export def plan-dir [directory: string]: nothing -> string {
  let seed = ($directory | path join seed)
  if ($seed | path join plan.json | path exists) { $seed } else { $directory }
}

def output-path [drv: record, name: string]: nothing -> string {
  let path = ($drv.outputs | get $name | get -o path | default ($drv.env | get -o $name))
  if $path == null { error make {msg: $"missing ($name) output path for ($drv.name)"} }
  if $path starts-with '/' { $path } else { $"/nix/store/($path)" }
}

def declared-paths [graph: table]: nothing -> list<string> {
  $graph | each {|entry| [$entry.path] ++ $entry.drv.inputs.srcs }
    | flatten | uniq | each {|path| $"/nix/store/($path)" }
}

def resolved-paths [directory: string, graph: table, paths: list<string>]: nothing -> list<string> {
  let recipes = ($paths | where {|path| $path ends-with '.drv' })
  if ($recipes | is-empty) { return [] }
  let resolved = (capture nix ([derivation show] ++ $recipes ++ (store-args $directory)) | from json)
  if $resolved.version != 4 { error make {msg: "expected Nix derivation JSON version 4"} }
  $resolved.derivations | transpose path drv | where {|entry|
    $graph | where drv.name == $entry.drv.name | any {|original|
      if ($original.drv | reject inputs) != ($entry.drv | reject inputs) { false } else {
        let inputs = ($original.drv.inputs.drvs | transpose path spec | each {|dependency|
          let target = ($graph | where path == $dependency.path | first | get drv)
          $dependency.spec.outputs | each {|name| output-path $target $name | path basename }
        } | flatten | append $original.drv.inputs.srcs | sort | uniq)
        $entry.drv.inputs == {drvs: {}, srcs: $inputs}
      }
    }
  } | get path | each {|path| $"/nix/store/($path)" }
}

def checkpoints [graph: table, derivation: string]: nothing -> table {
  let names = [
    '^seed-llvm-static-' '^mrustc-bootstrap-' '^seed-rust-[0-9]'
    '^seed-nushell-static-' '^seed-jig-static-'
  ] | each {|pattern|
    let found = ($graph | where {|entry| $entry.drv.name =~ $pattern } | sort-by --natural drv.name)
    if ($found | is-empty) { error make {msg: $"missing bootstrap checkpoint matching ($pattern)"} }
    $found | get path
  } | flatten | append ($derivation | path basename)
  $names | each {|path|
    let drv = ($graph | where path == $path | first | get drv)
    let output = (output-path $drv out)
    let baseline = (if ($output | path exists) {
      let info = (capture nix [path-info --json --json-format "2" $output] | from json)
      if $info.version != 2 { error make {msg: "expected Nix path-info JSON version 2"} }
      $info.info | values | first | get narHash
    } else { null })
    {name: $drv.name, derivation: $"/nix/store/($path)", output: $output, baseline: $baseline}
  }
}

def freeze [directory: string, seed: bool]: nothing -> nothing {
  let metadata = (if $seed { $directory | path join seed } else { $directory })
  let previous = (if $seed {
    if not ($directory | path join prepared.json | path exists) {
      error make {msg: "complete the isolated LLVM build before extending it through the seed"}
    }
    let original = (open ($directory | path join plan.json))
    let physical = ($directory | path join store ($original.output | str trim --left --char '/'))
    if not ($physical | path exists) {
      error make {msg: "complete the isolated LLVM build before extending it through the seed"}
    }
    let hash = (capture nix [hash path $physical])
    if $original.baseline != null and $hash != $original.baseline {
      error make {msg: "isolated LLVM differs from its frozen baseline"}
    }
    $original
  } else {
    if ($directory | path join store | path exists) {
      error make {msg: "store exists without a bootstrap plan, choose a new --state directory"}
    }
    null
  })
  let repository = ($bootstrap_dir | path dirname)
  let target = (if $seed { "bootstrap-seed" } else { "bootstrap-llvm" })
  print $"Freezing ($target) and auditing its inputs"
  let drv = (capture nix [eval --raw $"($repository)#($target).drvPath"])
  let document = (capture nix [derivation show --recursive $drv] | from json)
  if $document.version != 4 { error make {msg: "expected Nix derivation JSON version 4"} }
  let graph = ($document.derivations | transpose path drv)
  let stages = ($graph | where {|entry| $entry.drv.name =~ '^stage0-posix-.*-source$' })
  if ($stages | length) != 1 { error make {msg: "expected one pinned stage0 source tree"} }
  let stage = ($stages | first)
  let source = {derivation: $"/nix/store/($stage.path)", path: $stage.drv.env.out, hash: $stage.drv.outputs.out.hash}
  if $seed {
    let original = (open ($directory | path join graph.json)).derivations
    if $source != $previous.source or ($original | columns | any {|path| $path not-in ($graph | get path) }) {
      error make {msg: "seed graph does not extend the frozen LLVM graph, keep this store and choose a new --state"}
    }
  }
  let allowed = (declared-paths $graph)
  let closure = (capture nix [path-info --recursive $drv] | lines)
  if ($closure | any {|path| $path not-in $allowed }) {
    error make {msg: "derivation closure contains paths outside the declared build inputs"}
  }
  let flags = (if $seed { [--rust] } else { [] })
  let report = (capture nu ([--no-config-file ($bootstrap_dir | path join audit.nu) $drv] ++ $flags) | from json)
  let output = (output-path ($document.derivations | get ($drv | path basename)) out)
  let baseline = (if ($output | path exists) { capture nix [hash path $output] } else { null })
  let plan = {derivation: $drv, output: $output, baseline: $baseline, source: $source, created: (date now)}
  let frozen = (if $seed { $plan | insert checkpoints (checkpoints $graph $drv) } else { $plan })
  mkdir $metadata
  $document | to json | save --force ($metadata | path join graph.json)
  $report | to json | save --force ($metadata | path join audit.json)
  $frozen | to json | save --force ($metadata | path join plan.tmp.json)
  mv --force ($metadata | path join plan.tmp.json) ($metadata | path join plan.json)
}

export def prepare [directory: string, through: string, --refresh]: nothing -> record {
  let active = (plan-dir $directory)
  if $through == llvm and $active != $directory {
    error make {msg: "this run has advanced through the seed, omit --through to resume it"}
  }
  if $refresh {
    if $active == $directory or not ($active | path join prepared.json | path exists) {
      error make {msg: "refresh requires a prepared seed plan"}
    }
    let original = (open ($active | path join plan.json))
    let archive = ($directory | path join history ($original.derivation | path basename))
    if ($archive | path exists) {
      error make {msg: "this plan is already archived, inspect its history before refreshing again"}
    }
    mkdir $archive
    for name in [plan.json graph.json audit.json prepared.json checkpoints.json] {
      let file = ($active | path join $name)
      if ($file | path exists) { cp $file ($archive | path join $name) }
    }
    freeze $directory true
    rm ($active | path join prepared.json)
    let comparisons = ($active | path join checkpoints.json)
    if ($comparisons | path exists) { rm $comparisons }
    print $"Previous plan and comparisons are preserved in ($archive)"
  }
  if $through == seed and $active == $directory {
    freeze $directory true
  } else if not ($active | path join plan.json | path exists) {
    freeze $directory false
  }
  let metadata = (plan-dir $directory)
  let plan = (open ($metadata | path join plan.json))
  if ($metadata | path join prepared.json | path exists) { return $plan }
  save-progress $directory {phase: preparing, target: $plan.derivation}
  let graph = ((open ($metadata | path join graph.json)).derivations | transpose path drv)
  let historical = (files ($directory | path join history '*' graph.json)
    | each {|file| (open $file).derivations | transpose path drv } | flatten | uniq-by path)
  let prior_outputs = (if $metadata != $directory {
    (open ($directory | path join graph.json)).derivations | values | each {|drv|
      $drv.outputs | columns | each {|name| output-path $drv $name }
    } | flatten
  } else { [] })
  let sources = ($graph | each {|entry|
    $entry.drv.outputs | transpose name spec | where spec.hash? != null | each {|output|
      {path: (output-path $entry.drv $output.name), hash: $output.spec.hash, method: $output.spec.method}
    }
  } | flatten | uniq-by path)
  let historical_outputs = ($historical | each {|entry|
    $entry.drv.outputs | columns | each {|name| output-path $entry.drv $name }
  } | flatten)
  mut allowed = ((declared-paths $graph) ++ (declared-paths $historical) ++ $historical_outputs
    ++ $prior_outputs ++ ($sources | get path) ++ [$plan.source.path])
  let store = ($directory | path join store)
  if ($store | path exists) {
    let existing = (capture nix ([path-info --all] ++ (store-args $directory)) | lines)
    let extra = ($existing | where {|path| $path not-in $allowed })
    $allowed = ($allowed ++ (resolved-paths $directory ($graph ++ $historical | uniq-by path) $extra))
    let unexpected = ($existing | where {|path| $path not-in $allowed })
    if ($unexpected | is-not-empty) {
      error make {msg: $"isolated store contains unexpected paths before preparation\n($unexpected | str join (char newline))"}
    }
  }
  print "Preparing the isolated bootstrap store"
  mkdir ($directory | path join build)
  capture nix [copy --derivation --no-check-sigs --option substitute "false"
    --to $store --out-link ($metadata | path join inputs) $plan.derivation] | ignore
  if not ($plan.source.path | path exists) {
    try { ^nix build $"($plan.source.derivation)^out" --no-link -L } catch {|failure|
      error make {msg: $"could not obtain the pinned stage0 source tree\n($failure.msg)"}
    }
  }
  if (capture nix [hash path $plan.source.path]) != $plan.source.hash {
    error make {msg: "stage0 source tree differs from its pinned hash"}
  }
  capture nix [copy --no-check-sigs --option substitute "false" --to $store $plan.source.path] | ignore
  mut imported = []
  if $metadata != $directory {
    let valid = (capture nix ([path-info --all] ++ (store-args $directory)) | lines)
    for source in ($sources | where {|entry| $entry.path not-in $valid and ($entry.path | path exists) }) {
      let command = (match $source.method { flat => "file", nar => "path", _ => {
        error make {msg: $"unsupported fixed-output hash method ($source.method)"}
      } })
      if (capture nix [hash $command $source.path]) != $source.hash {
        error make {msg: $"source input differs from its pinned hash ($source.path)"}
      }
      let closure = (capture nix [path-info --recursive $source.path] | lines)
      if $closure != [$source.path] {
        error make {msg: $"source input references other store paths ($source.path)"}
      }
      print $"Reusing verified source ($source.path)"
      capture nix [copy --no-check-sigs --option substitute "false" --to $store $source.path] | ignore
      $imported = ($imported | append $source)
    }
  }
  let valid = (capture nix ([path-info --all] ++ (store-args $directory)) | lines)
  if ($valid | any {|path| $path not-in $allowed }) {
    error make {msg: "isolated store contains unexpected paths after preparation"}
  }
  {paths: $valid, imported_sources: $imported, prepared: (date now)} | to json
    | save --force ($metadata | path join prepared.tmp.json)
  mv --force ($metadata | path join prepared.tmp.json) ($metadata | path join prepared.json)
  $plan
}

export def verify-stage [directory: string, stage: record]: nothing -> record {
  let physical = ($directory | path join store ($stage.output | str trim --left --char '/'))
  let hash = (capture nix [hash path $physical])
  let comparison = (if $stage.baseline == null { "unavailable" } else if $hash == $stage.baseline { "identical" } else { "different" })
  let result = {name: $stage.name, derivation: $stage.derivation, output: $physical, nar_hash: $hash, comparison: $comparison, verified: (date now)}
  let metadata = (plan-dir $directory)
  let file = ($metadata | path join checkpoints.json)
  let previous = (if ($file | path exists) { open $file } else { [] })
  $previous | where derivation != $stage.derivation | append $result | to json
    | save --force ($metadata | path join checkpoints.tmp.json)
  mv --force ($metadata | path join checkpoints.tmp.json) $file
  print $"Checkpoint ($stage.name), comparison with the original build is ($comparison)"
  if $comparison == different {
    print $"Output is preserved at ($physical), the mismatch will be reported after the remaining checkpoints"
  }
  $result
}
