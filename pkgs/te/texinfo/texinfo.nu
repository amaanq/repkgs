use core.nu *

# the shipped man pages are older than the patched scripts, and help2man is not around to redo them
export def keep-man []: nothing -> nothing {
   for page in (files $"((ctx).src)/man/*.1") { ^touch $page }
}
