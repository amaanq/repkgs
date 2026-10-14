use core.nu [act bin rewrite glob-files]

# --with-cross-build wants a native ICU build tree. The installed ICU stands in for bin/ and
# config/, and the tests' gentest comes from this build's tools/
export def cross-buildroot []: nothing -> nothing {
   let native = (bin icupkg | path dirname -n 2)
   let root   = $"($env.NIX_BUILD_TOP)/native-icu"
   mkdir $"($root)/config" $"($root)/bin" $"($root)/lib"
   for program in (glob-files --any $"($native)/bin/*") { ^ln -s $program $"($root)/bin/($program | path basename)" }
   ^ln -s $"($env.PWD)/tools" $"($root)/tools"

   let settings = {
      CROSS_ICU_VERSION: (act).spec.version
      TOOLEXEEXT: ""
      TOOLBINDIR: "$(cross_buildroot)/bin"
      TOOLLIBDIR: "$(cross_buildroot)/lib"
   }
   $settings | items {|key, value| $"($key)=($value)" } | str join "\n" | save -f $"($root)/config/icucross.mk"
   # left empty CURR_FULL_DIR stays unset, and data/Makefile uses the .res files this build made
   "" | save -f $"($root)/config/icucross.inc"
}

# lib/icu/<version>/{Makefile,pkgdata}.inc serve builds of ICU data against this install and
# spell out the prefix, which then follows the including Makefile
export def relative-prefix []: nothing -> nothing {
   let out = (act).out
   for makefile in (glob-files $"($out)/lib/icu/*/Makefile.inc") {
      rewrite $makefile {
         str replace $"prefix = ($out)" "prefix = $(abspath $(dir $(lastword $(MAKEFILE_LIST)))../../..)"
         | str replace -r '(?m)^SHELL = .*' "SHELL = /bin/sh"
      }
   }
   for pkgdata in (glob-files $"($out)/lib/icu/*/pkgdata.inc") {
      rewrite $pkgdata {
         str replace -a $"-I($out)/include" "-I$(prefix)/include"
         | str replace -a $"-install_name ($out)/" "-install_name $(prefix)/"
         | str replace -a $"-rpath,($out)/" "-rpath,$(prefix)/"
      }
   }
}
