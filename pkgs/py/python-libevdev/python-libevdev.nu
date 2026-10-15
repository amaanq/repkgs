use core.nu [act dep rewrite shared-name cue]
use glob.nu [glob-files]

# ctypes otherwise looks libevdev up through the loader's search path, which knows no store paths
export def pin-library []: nothing -> nothing {
   let lib = $"(dep libevdev 'the bindings load it through ctypes')/lib/(shared-name evdev 2)"

   rewrite libevdev/_clib.py { str replace --all '"libevdev.so.2"' $'"($lib)"' }
}

# upstream keeps its suite in test/, the python system's pytest option reads tests/
export def test []: nothing -> nothing {
   let c    = (act)
   let site = (glob-files --dirs $"($c.out)/lib/python3*/site-packages" | first)

   with-env {PYTHONPATH: $"($site):($env.PYTHONPATH)"} { cue python3 -m pytest -q $"($c.src)/test" }
}
