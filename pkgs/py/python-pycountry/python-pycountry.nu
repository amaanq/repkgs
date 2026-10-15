use core.nu [act cue]
use glob.nu [glob-files]

# upstream ships its suite inside the package, the python system's pytest option reads tests/. Its
# pytest config requires pytest-cov for coverage reports only, and puts src/ ahead of the install
export def test []: nothing -> nothing {
   let c    = (act)
   let site = (glob-files --dirs $"($c.out)/lib/python3*/site-packages" | first)
   let opts = [ "-o" "required_plugins=" "-o" "addopts=--doctest-glob=*.rst" "-o" "pythonpath=" ]

   with-env {PYTHONPATH: $"($site):($env.PYTHONPATH)"} {
      cue python3 -m pytest -q ...$opts $"($c.src)/src/pycountry/tests" $"($c.src)/README.rst"
   }
}
