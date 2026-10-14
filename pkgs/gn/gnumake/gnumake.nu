use core.nu [act]

export def drop-general4 []: nothing -> nothing {
   rm $"((act).src)/tests/scripts/misc/general4"
}
