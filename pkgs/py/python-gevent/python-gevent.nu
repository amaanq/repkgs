use core.nu [act cue]
use glob.nu [glob-files]

# the sandbox has no network, so leave out the tests that need it
export def test []: nothing -> nothing {
   let c    = (act)
   let site = (glob-files $"($c.out)/lib/python3*/site-packages" | first)

   with-env {PYTHONPATH: $"($site):($env.PYTHONPATH)"} { cue python3 -m gevent.tests -j ($c.njobs | into string) -u all,-network }
}
