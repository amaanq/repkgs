use core.nu [act cue]
use glob.nu [glob-files]

export def test []: nothing -> nothing {
   let c    = (act)
   let site = (glob-files $"($c.out)/lib/python3*/site-packages" | first)

   cd $c.src
   with-env {PYTHONPATH: $"($site):($env.PYTHONPATH)"} { cue python3 -m unittest -v tests }
}
