use core.nu [act cue]
use glob.nu [glob-files]

# upstream keeps its suite in testing/, the python system's pytest option reads tests/. The python
# system exports PYTHONDONTWRITEBYTECODE, and test_dont_write_bytecode asserts a plain gateway
# starts without it
export def test []: nothing -> nothing {
   let c    = (act)
   let site = (glob-files $"($c.out)/lib/python3*/site-packages" | first)

   hide-env PYTHONDONTWRITEBYTECODE
   with-env {PYTHONPATH: $"($site):($env.PYTHONPATH)"} { cue python3 -m pytest -q $"($c.src)/testing" }
}
