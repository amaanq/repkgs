use core.nu *

export def --env configure-env []: nothing -> nothing {
  # configure takes the .pc directory from pkg-config's search path otherwise
  $env.PKG_CONFIG_LIBDIR = $"((ctx).out)/lib/pkgconfig"
  # and guesses gcc for the build machine
  $env.BUILD_CC = $env.CC_FOR_BUILD
}

# -lncurses, -ltinfo and the .pc names resolve to the wide-character libraries
export def narrow-names []: nothing -> nothing {
  let c     = (ctx)
  let lib   = $"($c.out)/lib"
  let names = [ncurses form panel menu] | append (if $c.platform.os == "windows" { [] } else { [tinfo] })
  for name in $names {
    ^ln -sf (linklib $"($name)w") $"($lib)/(linklib $name)"
    if $c.platform.binfmt != "coff" { ^ln -sf (shlib $"($name)w" 6) $"($lib)/(shlib $name 6)" }
    ^ln -sf $"($name)w.pc" $"($lib)/pkgconfig/($name).pc"
  }
  # an sh script repeating what the .pc files say
  rm $"($c.out)/bin/ncursesw6-config"
}
