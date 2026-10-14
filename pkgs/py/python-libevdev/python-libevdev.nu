use core.nu [ctx dep-root edit shlib cue]
use glob.nu [files]

# ctypes otherwise looks libevdev up through the loader's search path, which knows no store paths
export def pin-library []: nothing -> nothing {
   let lib = $"(dep-root libevdev 'the bindings load it through ctypes')/lib/(shlib evdev 2)"

   edit libevdev/_clib.py { str replace --all '"libevdev.so.2"' $'"($lib)"' }
}

# upstream keeps its suite in test/, the python system's pytest option reads tests/
export def test []: nothing -> nothing {
   let c    = (ctx)
   let site = (files $"($c.out)/lib/python3*/site-packages" | first)

   with-env {PYTHONPATH: $"($site):($env.PYTHONPATH)"} { cue python3 -m pytest -q $"($c.src)/test" }
}
