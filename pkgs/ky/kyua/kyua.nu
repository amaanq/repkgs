use core.nu [act]

export def --env check-config []: nothing -> nothing {
   let config = $"($env.NIX_BUILD_TOP)/kyua-check.conf"
   $"syntax\(2)\nparallelism = ((act).njobs)\n" | save -f $config
   $env.KYUA_CONFIG_FILE_FOR_CHECK = $config
}

export def drop-tests []: nothing -> nothing { rm -r $"((act).out)/tests" }
