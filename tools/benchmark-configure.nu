#!/usr/bin/env nu

use ../builder/prepare.nu
use ../builder/systems/autotools.nu

def capture [program: string, arguments: list<string>]: nothing -> string {
  let result = (^$program ...$arguments | complete)
  if $result.exit_code != 0 { error make {msg: $result.stderr} }
  $result.stdout | str trim
}

def main [--seed: path, --daemon: path, --runs: int = 3, ...packages: string]: nothing -> nothing {
  if $seed == null or $daemon == null { error make {msg: "--seed and --daemon are required"} }
  if $runs < 1 { error make {msg: "--runs must be positive"} }
  let seed_path = ($seed | path expand --strict)
  let daemon_path = ($daemon | path expand --strict)
  let targets = (if ($packages | is-empty) { [sed diffutils] } else { $packages })
  let repository = ($env.FILE_PWD | path dirname)
  let directory = (capture mktemp [-d /tmp/repkgs-configure-benchmark.XXXXXXXX])
  let unit = $"repkgs-configure-($directory | path basename)"
  let socket = ($directory | path join socket)
  let shell = ($seed_path | path join bin/nu)
  print $"Results and logs in ($directory)"
  capture systemd-run [--user --collect $"--unit=($unit)" $"--setenv=XDG_CACHE_HOME=($directory)/cache"
    --setenv=JIGD_SLOTS=30 $daemon_path $socket] | ignore
  try {
    let deadline = (date now) + 5sec
    while not ($socket | path exists) {
      if (date now) >= $deadline { error make {msg: "benchmark daemon did not create its socket"} }
      sleep 25ms
    }
    mut results = []
    for package in $targets {
      let output = (capture nix-build [$repository -A $package --argstr seed $seed_path --no-out-link --cores "30" --max-jobs "1"])
      let drv = (capture nix-store [--query --deriver $output])
      let description = (capture nix [derivation show $drv] | from json | get derivations | values | first)
      let attributes = ($description.structuredAttrs | update outputs {out: $output})
      if $attributes.spec.uses != [autotools] { error make {msg: $"($package) is not an Autoconf package"} }
      let work = ($directory | path join $package)
      mkdir $work
      $attributes | to json | save ($work | path join original.json)
      let modes = [prime] ++ (1..$runs | each {|round| [$"baseline-($round)" $"shared-($round)"] } | flatten) ++ [whole-cold whole-warm]
      for mode in $modes {
        print $"($package) ($mode)"
        let log = ($directory | path join $"($package)-($mode).log")
        let arguments = [-i $"PATH=($seed_path)/bin" NIX_STORE=/nix/store NIX_BUILD_CORES=30
          $"NIX_BUILD_TOP=($work)" $"NIX_ATTRS_JSON_FILE=($work)/attrs.json" $"JIG_SOCK=($socket)"
          $shell --no-config-file '--experimental-options=[cell-path-types]' $env.CURRENT_FILE work $work $mode]
        try { ^env ...$arguments out+err> $log } catch { error make {msg: $"($package) ($mode) failed, see ($log)"} }
        let measured = (open ($work | path join result.json))
        $results = ($results | append ($measured | insert package $package))
        $results | to json | save --force ($directory | path join results.json)
      }
      let measurements = ($results | where package == $package)
      let baseline = ($measurements | first | get outputs)
      if ($measurements | any {|entry| $entry.outputs != $baseline }) {
        error make {msg: $"($package) generated different configure outputs, see ($directory)"}
      }
    }
    $results | reject outputs | print
  } catch {|failure|
    capture systemctl [--user stop $unit] | ignore
    error make {msg: $failure.msg}
  }
  capture systemctl [--user stop $unit] | ignore
}

def "main work" [directory: path, mode: string]: nothing -> nothing {
  let original = (open ($directory | path join original.json))
  let inputs = (if $mode starts-with shared {
    $original | update probeInputs { append $mode }
  } else { $original })
  $inputs | to json | save --force $env.NIX_ATTRS_JSON_FILE
  for name in [source build] {
    let path = ($directory | path join $name)
    if ($path | path exists) { rm --recursive $path }
  }
  prepare
  $env.PKGS_CTX = ($env.PKGS_CTX | update cache ($mode starts-with shared or $mode starts-with whole))
  $env.JIG_LOG = ($directory | path join $"($mode)-jig.log")
  cd (autotools workdir)
  autotools setup
  let elapsed = (timeit { autotools configure })
  let outputs = (glob **/{Makefile,config.h} | sort | each {|file|
    {file: ($file | path relative-to $directory), hash: (open --raw $file | hash sha256)}
  })
  let events = (if ($env.JIG_LOG | path exists) {
    open --raw $env.JIG_LOG | lines | parse "{tool}\t{outcome}\t{subject}\t{milliseconds}"
  } else { [] })
  {mode: $mode, seconds: (($elapsed | into int) / 1_000_000_000), outputs: $outputs
    headers: ($events | where tool == header | group-by outcome | transpose outcome events
      | each {|group| {outcome: $group.outcome, count: ($group.events | length)} })}
    | to json | save --force ($directory | path join result.json)
}
