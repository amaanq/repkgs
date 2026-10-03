#!/usr/bin/env nu

def capture [program: string, arguments: list<string>]: nothing -> string {
   let result = (^$program ...$arguments | complete)
   if $result.exit_code != 0 { error make {msg: $result.stderr} }
   $result.stdout | str trim
}

def main [--peanut: path, --daemon: path, --runs: int = 3, ...packages: string]: nothing -> nothing {
   if $peanut == null or $daemon == null { error make {msg: "--peanut and --daemon are required"} }
   if $runs < 1 { error make {msg: "--runs must be positive"} }
   let peanut_path = ($peanut | path expand --strict)
   let daemon_path = ($daemon | path expand --strict)
   let targets = (if ($packages | is-empty) { [sed diffutils] } else { $packages })
   let repository = ($env.FILE_PWD | path dirname)
   let directory = (capture mktemp [-d /tmp/motley-configure-benchmark.XXXXXXXX])
   let unit = $"motley-configure-($directory | path basename)"
   let socket = ($directory | path join socket)
   let shell = (which nu | first | get path | path expand)
   {peanut: $peanut_path, daemon: $daemon_path, daemon_hash: (capture nix [hash file $daemon_path]), runs: $runs, packages: $targets, started: (date now)}
      | to json | save ($directory | path join environment.json)
   print $"Results and logs in ($directory)"
   capture systemd-run [--user --collect $"--unit=($unit)" $"--setenv=XDG_CACHE_HOME=($directory)/cache"
      --setenv=MUMMERD_SLOTS=30 $daemon_path $socket] | ignore
   try {
      let deadline = (date now) + 5sec
      while not ($socket | path exists) {
         if (date now) >= $deadline { error make {msg: "benchmark daemon did not create its socket"} }
         sleep 25ms
      }
      mut results = []
      let builds = ($targets | each {|package|
         let log = ($directory | path join $"($package)-build.log")
         print $"Preparing ($package), build log in ($log)"
         let built = (^nix-build $repository -A $package --argstr peanut $peanut_path --no-out-link --cores 30 --max-jobs 1 --keep-failed err> $log | complete)
         if $built.exit_code != 0 { error make {msg: $"Could not build ($package), see ($log)"} }
         let output = ($built.stdout | str trim)
         let drv = (capture nix-store [--query --deriver $output])
         let description = (capture nix [derivation show $drv] | from json | get derivations | values | first)
         let attributes = ($description.structuredAttrs | update outputs {out: $output})
         if $attributes.spec.uses != [autotools] { error make {msg: $"($package) is not an Autoconf package"} }
         {package: $package, attributes: $attributes}
      })
      for build in $builds {
         let package = $build.package
         let work = ($directory | path join $package)
         mkdir $work
         $build.attributes | to json | save ($work | path join original.json)
         let modes = [prime] ++ (1..$runs | each {|round| [$"baseline-($round)" $"shared-($round)"] } | flatten) ++ [whole-cold whole-warm]
         for mode in $modes {
            print $"($package) ($mode)"
            let log = ($directory | path join $"($package)-($mode).log")
            let arguments = [-i $"PATH=($peanut_path)/bin" NIX_STORE=/nix/store NIX_BUILD_CORES=30
               $"NIX_BUILD_TOP=($work)" $"NIX_ATTRS_JSON_FILE=($work)/attrs.json" $"MUMMER_SOCK=($socket)"
               $shell --no-config-file '--experimental-options=[cell-path-types]' $env.CURRENT_FILE work $work $mode]
            try { ^env ...$arguments out+err> $log } catch { error make {msg: $"($package) ($mode) failed, see ($log)"} }
            let caches = (open --raw $log | lines | where $it starts-with '@nix '
               | each {|line| $line | str substring 5.. | from json } | where action == msg
               | get msg | parse 'config.cache: {status}' | get status)
            if ($caches | length) != 1 { error make {msg: $"($package) ($mode) did not report its configure cache state, see ($log)"} }
            let cache = ($caches | first)
            let expected = (if $mode == whole-warm { "restored" } else { "cold" })
            if $cache != $expected { error make {msg: $"($package) ($mode) expected a ($expected) configure cache, got ($cache)"} }
            let measured = (open ($work | path join result.json) | insert configure_cache $cache)
            if ($measured.outputs | is-empty) { error make {msg: $"($package) generated no Makefile or config.h to compare"} }
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
      let summary = ($results | group-by package --to-table | each {|group|
         let measurements = $group.items
         let baseline = ($measurements | where mode starts-with baseline | get seconds | math median)
         let shared = ($measurements | where mode starts-with shared | get seconds | math median)
         let warm = ($measurements | where mode == whole-warm | first | get seconds)
         {package: $group.package, compiler_cache_seconds: $baseline, shared_headers_seconds: $shared
            configure_cache_seconds: $warm, configure_speedup: ($baseline / $warm)}
      })
      $summary | to json | save ($directory | path join summary.json)
      $summary | print
   } catch {|failure|
      capture systemctl [--user stop $unit] | ignore
      error make {msg: $failure.rendered}
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
   let prepared = (^bigtop prepare | from json)
   load-env $prepared.env
   cd $prepared.cwd
   let ctx = ($env.NIX_BUILD_TOP | path join ctx.json)
   let context = (open $ctx)
   $context | update cache ($mode starts-with shared or $mode starts-with whole) | save --force $ctx
   $env.MUMMER_LOG = ($directory | path join $"($mode)-mummer.log")
   load-env (^bigtop system setup autotools | from json)
   cd (^bigtop system workdir autotools | str trim)
   let elapsed = (timeit { ^bigtop system phase autotools configure })
   let outputs = (glob **/{Makefile,config.h} | sort | each {|file|
      {file: ($file | path relative-to $directory), hash: (open --raw $file | hash sha256)}
   })
   let events = (if ($env.MUMMER_LOG | path exists) {
      open --raw $env.MUMMER_LOG | lines | parse "{tool}\t{outcome}\t{subject}\t{milliseconds}"
   } else { [] })
   {mode: $mode, seconds: (($elapsed | into int) / 1_000_000_000), outputs: $outputs
      headers: ($events | where tool == header | group-by outcome | transpose outcome events
         | each {|group| {outcome: $group.outcome, count: ($group.events | length)} })}
      | to json | save --force ($directory | path join result.json)
}
