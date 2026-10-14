use core.nu [act cue]
use glob.nu [glob-files]

export def test []: nothing -> nothing {
   let site = (glob-files $"((act).out)/lib/python3*/site-packages" | first)

   with-env {PYTHONPATH: $"($site):($env.PYTHONPATH)"} { cue python3 -m unittest discover -v -s $"($site)/zope/interface" -t $site }
}
