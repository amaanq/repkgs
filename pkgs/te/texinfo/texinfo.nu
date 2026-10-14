use core.nu [act test-locale glob-files]

# the shipped man pages are older than the patched scripts, and help2man is not around to redo them
export def keep-man []: nothing -> nothing {
   for page in (glob-files $"((act).src)/man/*.1") { ^touch $page }
}

# document strings translate only under a locale other than C, which texinfo looks for as en_US
export def --env locale []: nothing -> nothing {
   test-locale en_US UTF-8
}
