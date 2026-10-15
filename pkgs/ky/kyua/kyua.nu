use core.nu [act]

export def drop-tests []: nothing -> nothing { rm -r $"((act).out)/tests" }
