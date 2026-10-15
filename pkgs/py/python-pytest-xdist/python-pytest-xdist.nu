use core.nu [act cue]
use glob.nu [glob-files]

# upstream keeps its suite in testing/, the python system's pytest option reads tests/
export def test []: nothing -> nothing {
   let c    = (act)
   let site = (glob-files --dirs $"($c.out)/lib/python3*/site-packages" | first)

   with-env {PYTHONPATH: $"($site):($env.PYTHONPATH)"} { cue python3 -m pytest -q $"($c.src)/testing" }
}
