def state-dir [directory: any]: nothing -> string {
  let base = ($env.XDG_STATE_HOME? | default ($env.HOME | path join .local/state))
  let path = ($directory | default ($base | path join repkgs/bootstrap-llvm) | path expand)
  if $path =~ '[%\r\n]' { error make {msg: "bootstrap state path contains unsupported characters"} }
  $path
}

def unit-name [directory: string]: nothing -> string {
  $"repkgs-bootstrap-($directory | hash sha256 | str substring 0..11).service"
}

def capture [program: string, arguments: list<string>]: nothing -> string {
  let result = (^$program ...$arguments | complete)
  if $result.exit_code != 0 {
    error make {msg: $"($program) exited with code ($result.exit_code)\n($result.stderr)"}
  }
  $result.stdout | str trim
}

def unit-info [directory: string]: nothing -> record {
  capture systemctl [--user show (unit-name $directory)
    --property=LoadState --property=ActiveState --property=SubState
    --property=Result --property=MainPID]
  | lines | split column --number 2 '=' key value | transpose --header-row --as-record
}

def save-progress [directory: string, progress: record]: nothing -> nothing {
  $progress | merge {updated: (date now)} | to json
    | save --force ($directory | path join progress.tmp.json)
  mv --force ($directory | path join progress.tmp.json) ($directory | path join progress.json)
}

def store-args [directory: string, --shell: string = "", --build-dir: string = ""]: nothing -> list<string> {
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

def failure-context [directory: string]: nothing -> string {
  let log = ($directory | path join build.log)
  if not ($log | path exists) { return "" }
  let matches = (^rg --line-number '^(Starting bootstrap|Building /nix/store/|error:)' $log | complete)
  if $matches.exit_code > 1 { error make {msg: $matches.stderr} }
  let errors = ($matches.stdout | lines | reverse
    | take until {|line| $line =~ '^\d+:(Starting bootstrap|Building /nix/store/)' } | reverse)
  if ($errors | is-empty) { return "" }
  let number = ($errors | first | split row ':' | first | into int)
  let start = ([($number - 20) 1] | math max)
  let excerpt = (^tail --lines $"+($start)" $log | ^head --lines 26 | complete)
  $excerpt.stdout | str trim
}

def build-stage [directory: string, derivation: string, cores: int, jobs: int, --shell: string = "", --build-dir: string = "", --dependencies]: nothing -> nothing {
  let installables = (if $dependencies {
    let graph = (open ($directory | path join graph.json)).derivations
    $graph | get ($derivation | path basename) | get inputs.drvs | transpose path spec
      | each {|entry| $"/nix/store/($entry.path)^($entry.spec.outputs | str join ',')" }
  } else { [$"($derivation)^out"] })
  let links = (if $dependencies { [--no-link] } else { [--out-link ($directory | path join result)] })
  save-progress $directory {phase: building, target: $derivation, dependencies: $dependencies, cores_per_build: $cores, jobs: $jobs}
  print $"Building ($derivation)"
  if $dependencies { print $"Building dependencies with ($jobs) jobs and ($cores) cores per build" }
  if $shell != "" { print $"Sandbox /bin/sh uses bootstrapped ($shell)" }
  let arguments = ([build] ++ $installables ++ [
    --cores $cores --max-jobs $jobs --keep-failed --option max-silent-time 0 --option timeout 0
    --json -L] ++ $links ++ (store-args $directory --shell $shell --build-dir $build_dir))
  ^nix ...$arguments out> ($directory | path join result.tmp.json)
}

def build-coreutils [directory: string, graph: table, shell: string, cores: int, jobs: int]: nothing -> nothing {
  let filesystem = (capture findmnt [--noheadings --output FSTYPE --target ($directory | path join build)])
  if $filesystem != bcachefs { return }
  let packages = ($graph | where {|entry|
    $entry.drv.name =~ '^coreutils-' and $entry.drv.structuredAttrs?.doCheck? == true
  } | each {|entry|
    let path = $"/nix/store/($entry.path)"
    let closure = (capture nix ([path-info --recursive $path] ++ (store-args $directory)) | lines | length)
    {path: $path, closure: $closure}
  } | sort-by closure)
  if ($packages | is-empty) { return }
  if (capture findmnt [--noheadings --output FSTYPE --target /tmp]) != tmpfs {
    error make {msg: "Coreutils filesystem checks on bcachefs need /tmp on tmpfs"}
  }
  let scratch = (capture mktemp [-d /tmp/repkgs-coreutils.XXXXXXXX])
  print $"Coreutils filesystem checks use ($scratch), completed outputs stay in the isolated store"
  for package in $packages {
    build-stage $directory $package.path ($cores // $jobs) $jobs --shell $shell --dependencies
    build-stage $directory $package.path $cores 1 --shell $shell --build-dir $scratch
  }
  rm --recursive $scratch
}

def prepare [directory: string]: nothing -> record {
  let plan_file = ($directory | path join plan.json)
  if not ($plan_file | path exists) {
    if ($directory | path join store | path exists) {
      error make {msg: "store exists without a bootstrap plan, choose a new --state directory"}
    }
    let repository = ($env.FILE_PWD | path dirname)
    let drv = (capture nix [eval --raw $"($repository)#bootstrap-llvm.drvPath"])
    let document = (capture nix [derivation show --recursive $drv] | from json)
    if $document.version != 4 { error make {msg: "expected Nix derivation JSON version 4"} }
    let graph = ($document.derivations | transpose path drv)
    let stages = ($graph | where {|entry| $entry.drv.name =~ '^stage0-posix-.*-source$' })
    if ($stages | length) != 1 { error make {msg: "expected one pinned stage0 source tree"} }
    let stage = ($stages | first)
    let allowed = ($graph | each {|entry| [$entry.path] ++ $entry.drv.inputs.srcs }
      | flatten | uniq | each {|path| $"/nix/store/($path)" })
    let closure = (capture nix [path-info --recursive $drv] | lines)
    if ($closure | any {|path| $path not-in $allowed }) {
      error make {msg: "derivation closure contains paths outside the declared build inputs"}
    }
    let report = (capture nu [($env.FILE_PWD | path join audit.nu) $drv] | from json)
    let output = $"/nix/store/($document.derivations | get ($drv | path basename) | get outputs.out.path)"
    let baseline = (if ($output | path exists) { capture nix [hash path $output] } else { null })
    let plan = {
      derivation: $drv
      output: $output
      baseline: $baseline
      source: {derivation: $"/nix/store/($stage.path)", path: $stage.drv.env.out, hash: $stage.drv.outputs.out.hash}
      created: (date now)
    }
    $document | to json | save --force ($directory | path join graph.json)
    $report | to json | save --force ($directory | path join audit.json)
    $plan | to json | save --force ($directory | path join plan.tmp.json)
    mv --force ($directory | path join plan.tmp.json) $plan_file
  }
  let plan = (open $plan_file)
  if ($directory | path join prepared.json | path exists) { return $plan }
  save-progress $directory {phase: preparing}
  print "Preparing the isolated LLVM bootstrap"
  mkdir ($directory | path join build)
  let store = ($directory | path join store)
  capture nix [copy --derivation --no-check-sigs --option substitute "false"
    --to $store --out-link ($directory | path join inputs) $plan.derivation] | ignore
  if not ($plan.source.path | path exists) {
    try { ^nix build $"($plan.source.derivation)^out" --no-link -L } catch {|failure|
      error make {msg: $"could not obtain the pinned stage0 source tree\n($failure.msg)"}
    }
  }
  if (capture nix [hash path $plan.source.path]) != $plan.source.hash {
    error make {msg: "stage0 source tree differs from its pinned hash"}
  }
  capture nix [copy --no-check-sigs --option substitute "false" --to $store $plan.source.path] | ignore
  let valid = (capture nix ([path-info --all] ++ (store-args $directory)) | lines)
  let document = (open ($directory | path join graph.json))
  let allowed = ($document.derivations | transpose path drv
    | each {|entry| [$entry.path] ++ $entry.drv.inputs.srcs } | flatten | uniq
    | each {|path| $"/nix/store/($path)" } | append $plan.source.path)
  if ($valid | any {|path| $path not-in $allowed }) {
    error make {msg: "isolated store contains unexpected paths before the first build"}
  }
  {paths: $valid, prepared: (date now)} | to json | save --force ($directory | path join prepared.json)
  $plan
}

def main []: nothing -> nothing { help main }

def "main start" [--state: path, --cores: int = 30, --jobs: int = 3]: nothing -> nothing {
  if $cores < 1 or $jobs < 1 { error make {msg: "cores and jobs must be positive"} }
  if $jobs > $cores { error make {msg: "jobs must not exceed the total core budget"} }
  let directory = (state-dir $state)
  mkdir $directory
  let unit = (unit-info $directory)
  if $unit.SubState == running or $unit.ActiveState in [activating deactivating] {
    print "Bootstrap is already running"
    main status --state $directory
    return
  }
  if $unit.ActiveState == failed {
    capture systemctl [--user reset-failed (unit-name $directory)] | ignore
  } else if $unit.LoadState != "not-found" {
    capture systemctl [--user stop (unit-name $directory)] | ignore
  }
  let shell = (which nu | first | get path | path expand)
  let lock = (which flock | first | get path | path expand)
  let script = ($env.CURRENT_FILE | path expand)
  capture systemd-run [--user --unit (unit-name $directory)
    --property=RemainAfterExit=yes --property=KillSignal=SIGINT --property=TimeoutStopSec=60
    $"--property=WorkingDirectory=($directory)"
    $"--property=StandardOutput=append:($directory)/build.log"
    --property=StandardError=inherit
    $lock --no-fork --nonblock ($directory | path join lock)
    $shell --no-config-file $script work $directory ($cores | into string) ($jobs | into string)] | ignore
  print $"Started (unit-name $directory) with ($cores) total cores and ($jobs) build jobs"
  print $"Log is ($directory)/build.log"
}

def "main work" [directory: path, cores: int, jobs: int]: nothing -> nothing {
  print $"Starting bootstrap (date now)"
  try {
    let plan = (prepare $directory)
    print "Compiled substitutes and remote builders are disabled"
    let graph = ((open ($directory | path join graph.json)).derivations | transpose path drv)
    let cores_per_build = ($cores // $jobs)
    mut shell = ""
    for name in [bash-2.05b bash-static-5.3] {
      let candidates = ($graph | where drv.name == $name)
      if ($candidates | length) != 1 { error make {msg: $"expected one ($name) in the frozen bootstrap graph"} }
      let candidate = ($candidates | first)
      build-stage $directory $"/nix/store/($candidate.path)" $cores_per_build $jobs --shell $shell
      $shell = $"/nix/store/($candidate.drv.outputs.out.path)/bin/bash"
    }
    build-coreutils $directory $graph $shell $cores $jobs
    build-stage $directory $plan.derivation $cores_per_build $jobs --shell $shell --dependencies
    build-stage $directory $plan.derivation $cores 1 --shell $shell
    mv --force ($directory | path join result.tmp.json) ($directory | path join result.json)
    let physical = ($directory | path join store ($plan.output | str trim --left --char '/'))
    let hash = (capture nix [hash path $physical])
    let comparison = (if $plan.baseline == null { "unavailable" } else if $hash == $plan.baseline { "identical" } else { "different" })
    save-progress $directory {phase: complete, output: $physical, nar_hash: $hash, comparison: $comparison}
    if $comparison == different {
      error make {msg: $"LLVM built but its NAR hash ($hash) differs from the original ($plan.baseline), output is preserved at ($physical)"}
    }
    print $"LLVM bootstrap complete, comparison with the original build is ($comparison)"
  } catch {|failure|
    save-progress $directory {phase: failed, error: $failure.msg}
    let context = (failure-context $directory)
    if $context != "" { print $"First build failure\n($context)" }
    error make {msg: $failure.msg}
  }
}

def "main status" [--state: path]: nothing -> nothing {
  let directory = (state-dir $state)
  let progress = (if ($directory | path join progress.json | path exists) {
    open ($directory | path join progress.json)
  } else { {phase: "not started"} })
  let unit = (unit-info $directory)
  let phase = (if $progress.phase in [preparing building] and $unit.ActiveState not-in [active activating] {
    "interrupted"
  } else { $progress.phase })
  mut counts = {}
  if ($directory | path join prepared.json | path exists) {
    let valid = (capture nix ([path-info --all] ++ (store-args $directory)) | lines)
    let graph = ((open ($directory | path join graph.json)).derivations | values)
    let done = ($graph | where {|drv|
      $drv.outputs | transpose name spec | all {|output|
        let path = ($output.spec.path? | default ($drv.env | get -o $output.name))
        $path != null and (if $path starts-with '/' { $path } else { $"/nix/store/($path)" }) in $valid
      }
    } | length)
    $counts = {completed_derivations: $done, total_derivations: ($graph | length)}
  }
  $progress | merge $counts | merge {phase: $phase, service: $unit.ActiveState, process: $unit.SubState, state: $directory, log: ($directory | path join build.log)} | print
}

def "main log" [--state: path, --lines: int = 40]: nothing -> nothing {
  let directory = (state-dir $state)
  let log = ($directory | path join build.log)
  if not ($log | path exists) { error make {msg: "no bootstrap log yet, run start first"} }
  let unit = (unit-info $directory)
  if $unit.SubState == running {
    ^tail --lines $lines --follow=name --retry $"--pid=($unit.MainPID)" $log
  } else if $unit.ActiveState != failed { ^tail --lines $lines $log }
  let finished = (unit-info $directory)
  if $finished.ActiveState == failed {
    let context = (failure-context $directory)
    if $context == "" { ^tail --lines $lines $log } else { print $"First build failure\n($context)" }
    print "Bootstrap stopped, completed derivations are preserved. Run start to resume."
  }
}

def "main stop" [--state: path]: nothing -> nothing {
  let directory = (state-dir $state)
  capture systemctl [--user stop (unit-name $directory)] | ignore
  if ($directory | path join progress.json | path exists) {
    let progress = (open ($directory | path join progress.json))
    if $progress.phase != complete { save-progress $directory {phase: stopped} }
  }
  print "Stopped the bootstrap, completed derivations remain in its store"
}
