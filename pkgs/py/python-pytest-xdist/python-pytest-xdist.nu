use core.nu [ctx cue]
use glob.nu [files]

# upstream keeps its suite in testing/, the python system's pytest option reads tests/
export def test []: nothing -> nothing {
   let c    = (ctx)
   let site = (files $"($c.out)/lib/python3*/site-packages" | first)

   with-env {PYTHONPATH: $"($site):($env.PYTHONPATH)"} { cue python3 -m pytest -q $"($c.src)/testing" }
}
