use core.nu [act glob-files]

# rocks with C modules then build with the cc on PATH, not the one configure found
export def use-cc []: nothing -> nothing {
   let sysroot = (act).platform.sysroot

   for config in (glob-files $"((act).out)/etc/luarocks/config-*.lua") {
      let lines = [
         ''
         'variables.CC = "cc"'
         'variables.LD = "cc"'
         $'external_deps_dirs = { "($sysroot)", "/usr/local", "/usr", "/" }'
         ''
      ]

      $lines | str join "\n" | save --append $config
   }
}
