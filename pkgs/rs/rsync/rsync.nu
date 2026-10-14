use core.nu [ctx edit fix-shebangs tool cue]
use glob.nu [files]

# the sandbox refuses setuid bits and user xattrs and has no fs.protected_regular. The daemon tests
# skip over the default pipe transport, and upstream runs them again on loopback TCP
export def test []: nothing -> nothing {
   let c        = (ctx)
   let python   = (tool python3)
   let bash     = (tool bash)
   let runtests = $"($c.src)/runtests.py"
   let args     = [ $"--rsync-bin=($c.build)/rsync" -j ($c.njobs | into string) "--exclude=chmod-option,chmod-setid,variety,variety-symlink-traversal,chown-fake,fake-super-backup-fifo-regression,protected-regular" ]

   # fix-shebangs skips the non-executable test scripts
   fix-shebangs $c.src
   for file in (files $"($c.src)/testsuite/*.py") {
      edit $file { str replace --all '#!/usr/bin/env python3' $"#!($python)" | str replace --all '#!/usr/bin/env bash' $"#!($bash)" }
   }

   cd $c.build
   cue make $"-j($c.njobs)" check-progs
   cue $runtests ...$args
   cue $runtests ...$args --use-tcp --daemon-tests-only
}
