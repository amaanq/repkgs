use core.nu [act]
use glob.nu [glob-files]

# the tests' subprocesses import psutil from the source tree, so point it at the
# installed one
export def --env test []: nothing -> nothing {
   let site = (glob-files --dirs $"((act).out)/lib/python3*/site-packages" | first)

   rm -r $"((act).src)/psutil"
   ln -s $"($site)/psutil" $"((act).src)/psutil"
   $env.PYTHONMALLOC = "malloc"
}
