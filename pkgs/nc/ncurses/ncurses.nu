use core.nu [act shared-name link-name]

export def --env configure-env []: nothing -> nothing {
   # configure takes the .pc directory from pkg-config's search path otherwise
   $env.PKG_CONFIG_LIBDIR = $"((act).out)/lib/pkgconfig"
   # and guesses gcc for the build machine
   $env.BUILD_CC = $env.CC_FOR_BUILD
}

# -lncurses, -ltinfo and the .pc names resolve to the wide-character libraries
export def narrow-names []: nothing -> nothing {
   let c     = (act)
   let lib   = $"($c.out)/lib"
   let names = [ncurses form panel menu] | append (if $c.platform.os == "windows" { [] } else { [tinfo] })
   for name in $names {
      ^ln -sf (link-name $"($name)w") $"($lib)/(link-name $name)"
      if $c.platform.binfmt != "coff" { ^ln -sf (shared-name $"($name)w" 6) $"($lib)/(shared-name $name 6)" }
      ^ln -sf $"($name)w.pc" $"($lib)/pkgconfig/($name).pc"
   }
   # an sh script repeating what the .pc files say
   rm $"($c.out)/bin/ncursesw6-config"
}
