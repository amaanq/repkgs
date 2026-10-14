use core.nu [act]

# leftovers from upstream's own test run
export def stale-logs []: nothing -> nothing {
   rm $"((act).src)/checkmk/test/check_checkmk.log" $"((act).src)/checkmk/test/check_checkmk.trs"
}
