use core.nu [ctx x]
use glob.nu [files]

# upstream keeps its suite in testing/, the python system's pytest option reads tests/. The python
# system exports PYTHONDONTWRITEBYTECODE, and test_dont_write_bytecode asserts a plain gateway
# starts without it
export def test []: nothing -> nothing {
   let c    = (ctx)
   let site = (files $"($c.out)/lib/python3*/site-packages" | first)

   hide-env PYTHONDONTWRITEBYTECODE
   with-env {PYTHONPATH: $"($site):($env.PYTHONPATH)"} { x python3 -m pytest -q $"($c.src)/testing" }
}
