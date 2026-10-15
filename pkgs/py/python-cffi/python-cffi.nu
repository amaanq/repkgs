use core.nu [act cue]
use glob.nu [glob-files]

# the suite lives at the sdist root next to the C backend tests, outside the installed package
export def test []: nothing -> nothing {
   let c    = (act)
   let site = (glob-files --dirs $"($c.out)/lib/python3*/site-packages" | first)

   cd $c.src
   with-env {PYTHONPATH: $"($site):($env.PYTHONPATH)"} { cue python3 -m pytest -q src/c testing }
}
