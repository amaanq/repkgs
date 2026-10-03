#!/usr/bin/env nu

use store.nu [capture save-progress store-args plan-dir prepare verify-stage]

def state-dir [directory: any]: nothing -> string {
  let base = ($env.XDG_STATE_HOME? | default ($env.HOME | path join .local/state))
  let path = ($directory | default ($base | path join manicpkgs/bootstrap-llvm) | path expand)
  if $path =~ '[%\r\n]' { error make {msg: "bootstrap state path contains unsupported characters"} }
  $path
}

def unit-name [directory: string]: nothing -> string {
  $"manicpkgs-bootstrap-($directory | hash sha256 | str substring 0..11).service"
}

def unit-info [directory: string]: nothing -> record {
  capture systemctl [--user show (unit-name $directory)
    --property=LoadState --property=ActiveState --property=SubState
    --property=Result --property=MainPID --property=MemoryPeak --property=MemoryMax]
  | lines | split column --number 2 '=' key value | transpose --header-row --as-record
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
    let graph = (open (plan-dir $directory | path join graph.json)).derivations
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
  let scratch = (capture mktemp [-d /tmp/manicpkgs-coreutils.XXXXXXXX])
  print $"Coreutils filesystem checks use ($scratch), completed outputs stay in the isolated store"
  for package in $packages {
    build-stage $directory $package.path ($cores // $jobs) $jobs --shell $shell --dependencies
    build-stage $directory $package.path $cores 1 --shell $shell --build-dir $scratch
  }
  rm --recursive $scratch
}

def main []: nothing -> nothing { help main }

def "main start" [--state: path, --cores: int = 30, --jobs: int = 3, --llvm-cores: int = 4, --memory: filesize = 48GiB, --through: string = "", --refresh]: nothing -> nothing {
  if $cores < 1 or $jobs < 1 or $llvm_cores < 1 { error make {msg: "cores and jobs must be positive"} }
  if $jobs > $cores { error make {msg: "jobs must not exceed the total core budget"} }
  if $llvm_cores > $cores { error make {msg: "LLVM workers must not exceed the total core budget"} }
  if $memory <= 0B { error make {msg: "memory limit must be positive"} }
  if $through not-in ["" llvm seed] { error make {msg: "--through must be llvm or seed"} }
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
  let maximum = ($memory | into int)
  let high = ($maximum * 4 // 5)
  capture systemd-run [--user --unit (unit-name $directory)
    --property=RemainAfterExit=yes --property=KillSignal=SIGINT --property=TimeoutStopSec=60
    $"--property=MemoryHigh=($high)" $"--property=MemoryMax=($maximum)"
    $"--property=WorkingDirectory=($directory)"
    $"--property=StandardOutput=append:($directory)/build.log"
    --property=StandardError=inherit
    $lock --no-fork --nonblock ($directory | path join lock)
    $shell --no-config-file $script work $directory ($cores | into string) ($jobs | into string) ($llvm_cores | into string) $through ($refresh | into string)] | ignore
  print $"Started (unit-name $directory) with ($cores) total cores and ($jobs) build jobs"
  print $"Checked LLVM builds use ($llvm_cores) workers, service memory limit is ($memory)"
  print $"Log is ($directory)/build.log"
}

def "main work" [directory: path, cores: int, jobs: int, llvm_cores: int, through: string, refresh: bool]: nothing -> nothing {
  print $"Starting bootstrap (date now)"
  let verified = (try {
    let plan = (prepare $directory $through --refresh=$refresh)
    print "Compiled substitutes and remote builders are disabled"
    let metadata = (plan-dir $directory)
    let graph = ((open ($metadata | path join graph.json)).derivations | transpose path drv)
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
    let checkpoints = (if $metadata == $directory { [$plan | insert name LLVM] } else { $plan.checkpoints })
    let checked_llvm = ($graph | where {|entry|
      ($entry.drv.name =~ '^llvm-[0-9]+\.[0-9]+\.[0-9]+$'
        and ($entry.drv.structuredAttrs?.doCheck? | default $entry.drv.env.doCheck?) in [true "1"])
    })
    mut results = []
    for stage in $checkpoints {
      print $"Starting checkpoint ($stage.name)"
      let closure = (capture nix ([path-info --recursive $stage.derivation] ++ (store-args $directory)) | lines)
      let valid = (capture nix ([path-info --all] ++ (store-args $directory)) | lines)
      for compiler in ($checked_llvm | where {|entry|
        ($"/nix/store/($entry.path)" in $closure
          and not ($entry.drv.outputs | values | all {|output| $"/nix/store/($output.path)" in $valid }))
      }) {
        let derivation = $"/nix/store/($compiler.path)"
        print $"Building ($compiler.drv.name) separately with ($llvm_cores) workers for compilation and tests"
        build-stage $directory $derivation $cores_per_build $jobs --shell $shell --dependencies
        build-stage $directory $derivation $llvm_cores 1 --shell $shell
      }
      build-stage $directory $stage.derivation $cores_per_build $jobs --shell $shell --dependencies
      build-stage $directory $stage.derivation $cores 1 --shell $shell
      $results = ($results | append (verify-stage $directory $stage))
    }
    mv --force ($directory | path join result.tmp.json) ($directory | path join result.json)
    $results
  } catch {|failure|
    save-progress $directory {phase: failed, error: $failure.msg}
    let context = (failure-context $directory)
    if $context != "" { print $"First build failure\n($context)" }
    error make {msg: $failure.msg}
  })
  let latest = ($verified | last)
  let mismatches = ($verified | where comparison == different | get name)
  let phase = (if ($mismatches | is-empty) { "complete" } else { "verification_failed" })
  save-progress $directory ($latest | merge {phase: $phase, mismatches: $mismatches})
  if ($mismatches | is-not-empty) {
    error make {msg: $"All bootstrap checkpoints built, but reproducibility verification failed for ($mismatches | str join ', '). Outputs are preserved, restarting will reuse them."}
  }
  print $"Bootstrap complete through ($latest.name), comparison with the original build is ($latest.comparison)"
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
  let metadata = (plan-dir $directory)
  if ($metadata | path join prepared.json | path exists) {
    let valid = (capture nix ([path-info --all] ++ (store-args $directory)) | lines)
    let graph = ((open ($metadata | path join graph.json)).derivations | values)
    let pending = ($graph | where {|drv|
      not ($drv.outputs | transpose name spec | all {|output|
        let path = ($output.spec.path? | default ($drv.env | get -o $output.name))
        $path != null and (if $path starts-with '/' { $path } else { $"/nix/store/($path)" }) in $valid
      })
    })
    $counts = {completed_derivations: (($graph | length) - ($pending | length)), total_derivations: ($graph | length)}
    if $phase in [complete verification_failed] and ($pending | is-not-empty) {
      $counts = ($counts | insert skipped_derivations ($pending | get name))
    }
  }
  if ($metadata | path join checkpoints.json | path exists) {
    let verified = (open ($metadata | path join checkpoints.json) | select name comparison)
    $counts = ($counts | insert checkpoints $verified)
  }
  let reason = (if $unit.Result == "oom-kill" { "out of memory" } else { $unit.Result })
  $progress | merge $counts | merge {phase: $phase, service: $unit.ActiveState, process: $unit.SubState, service_result: $reason, memory_peak_bytes: $unit.MemoryPeak, memory_limit_bytes: $unit.MemoryMax, state: $directory, log: ($directory | path join build.log)} | print
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
    let progress = (if ($directory | path join progress.json | path exists) {
      open ($directory | path join progress.json)
    } else { {} })
    if $progress.phase? == verification_failed {
      print $"All bootstrap outputs were built. Reproducibility differs for ($progress.mismatches | str join ', ')."
      print "The outputs and checkpoint comparisons are preserved. Restarting will not rebuild completed outputs."
      return
    }
    if $finished.Result == "oom-kill" {
      print "The kernel killed a bootstrap process for running out of memory"
      print "Nix may report this as interrupted by the user"
    }
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
