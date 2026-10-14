use core.nu [ctx tool edit files]

# --with-cross-build expects a native ICU build tree. Its data Makefiles only reach into it for the
# tools in bin/ and config/icucross.{mk,inc}, so the installed native ICU can stand in
export def cross-buildroot []: nothing -> nothing {
   let native = (tool icupkg | path dirname -n 2)
   let root   = $"($env.NIX_BUILD_TOP)/native-icu"
   mkdir $"($root)/config" $"($root)/bin" $"($root)/lib"
   for program in (files --any $"($native)/bin/*") { ^ln -s $program $"($root)/bin/($program | path basename)" }

   let settings = {
      CROSS_ICU_VERSION: (ctx).spec.version
      TOOLEXEEXT: ""
      TOOLBINDIR: "$(cross_buildroot)/bin"
      TOOLLIBDIR: "$(cross_buildroot)/lib"
      INVOKE: ""
      PKGDATA_INVOKE: ""
   }
   $settings | items {|key, value| $"($key)=($value)" } | str join "\n" | save -f $"($root)/config/icucross.mk"
   # left empty CURR_FULL_DIR stays unset, and data/Makefile uses the .res files this build made
   "" | save -f $"($root)/config/icucross.inc"
}

# lib/icu/<version>/{Makefile,pkgdata}.inc serve builds of ICU data against this install and
# spell out the prefix, which then follows the including Makefile
export def relative-prefix []: nothing -> nothing {
   let out = (ctx).out
   for makefile in (files $"($out)/lib/icu/*/Makefile.inc") {
      edit $makefile {
         str replace $"prefix = ($out)" "prefix = $(abspath $(dir $(lastword $(MAKEFILE_LIST)))../../..)"
         | str replace -r '(?m)^SHELL = .*' "SHELL = /bin/sh"
      }
   }
   for pkgdata in (files $"($out)/lib/icu/*/pkgdata.inc") {
      edit $pkgdata {
         str replace -a $"-I($out)/include" "-I$(prefix)/include"
         | str replace -a $"-install_name ($out)/" "-install_name $(prefix)/"
         | str replace -a $"-rpath,($out)/" "-rpath,$(prefix)/"
      }
   }
}
