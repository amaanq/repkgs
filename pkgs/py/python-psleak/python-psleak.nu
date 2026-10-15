use core.nu [act cue]
use glob.nu [glob-files]

# the leak tests want the libc allocator and an extension built in place
export def --env test []: nothing -> nothing {
   let site = (glob-files --dirs $"((act).out)/lib/python3*/site-packages" | first)

   do {
      cd (act).src
      rm psleak.py
      ln -s $"($site)/psleak.py" psleak.py
      with-env {PYTHONPATH: $"($site):($env.PYTHONPATH)"} { cue python3 tests/build_ext.py build_ext --inplace }
   }

   $env.PYTHONMALLOC = "malloc"
}
