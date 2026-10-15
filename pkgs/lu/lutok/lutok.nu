use core.nu [act cue]

export def drop-tests []: nothing -> nothing { rm -r $"((act).out)/tests" }

export def test []: nothing -> nothing {
   let c       = (act)
   let results = $"($c.build)/results.db"
   try {
      cue kyua --config=none test $"--kyuafile=($c.src)/Kyuafile" $"--build-root=($c.build)" $"--results-file=($results)"
   } catch {
      cue kyua report --verbose $"--results-file=($results)" --results-filter=broken,failed
      error make {msg: "kyua: tests failed"}
   }
}
