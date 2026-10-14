use core.nu [ctx]

export def drop-general4 []: nothing -> nothing {
   rm $"((ctx).src)/tests/scripts/misc/general4"
}
