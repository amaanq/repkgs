use core.nu *

# fix-shebangs skips the non-executable test scripts
export def test-scripts []: nothing -> nothing {
   let c      = (ctx)
   let python = (tool python3)
   let bash   = (tool bash)

   fix-shebangs $c.src
   for file in (files $"($c.src)/testsuite/*.py") {
      edit $file { str replace --all '#!/usr/bin/env python3' $"#!($python)" | str replace --all '#!/usr/bin/env bash' $"#!($bash)" }
   }
}

# the daemon tests skip over the default pipe transport, upstream runs them again on loopback TCP
export def test-tcp []: nothing -> nothing {
   let c       = (ctx)
   let exclude = (options autotools | get testTarget | parse -r '(?<flag>--exclude=\S+)' | get flag)

   cd $c.build
   x $"($c.src)/runtests.py" $"--rsync-bin=($c.build)/rsync" -j ($c.njobs | into string) --use-tcp --daemon-tests-only ...$exclude
}
