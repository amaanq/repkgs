use core.nu [act cue]

export def stash-tests []: nothing -> nothing {
   let c = (act)
   mv $"($c.out)/tests" $"($c.build)/installed-tests"
}

export def test []: nothing -> nothing {
   let c       = (act)
   let staged  = $"($c.build | path dirname)/prefix"
   let results = $"($c.build)/results.db"
   if not ($staged | path exists) { ^ln -s $c.out $staged }

   cd $"($c.build)/installed-tests/atf"
   try {
      cue kyua --config=none test $"--results-file=($results)"
   } catch {
      cue kyua report --verbose $"--results-file=($results)" --results-filter=broken,failed
      error make {msg: "kyua: tests failed"}
   }
}
