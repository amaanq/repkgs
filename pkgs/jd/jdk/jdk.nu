use core.nu [act dep build-dep bin cue rewrite glob-files]

export def configure []: nothing -> nothing {
   let c = (act)

   # jpackage uses std::nothrow but never includes <new>, relying on <string> to drag it in as
   # libstdc++ and older libc++ do. Every jpackage file includes tstrings.h, so <new> goes there
   let header = "src/jdk.jpackage/share/native/common/tstrings.h"
   rewrite $header { str replace "#include <string>" "#include <new>\n#include <string>" }

   let cross = if $c.platform.cross {
      [
         $"--openjdk-target=($c.platform.gnuTriple)"
         $"--with-build-jdk=(build-dep jdk)"
         $"BUILD_CC=($env.CC_FOR_BUILD)"
         $"BUILD_CXX=($env.CXX_FOR_BUILD)"
      ]
   } else { [] }
   let bundled = [giflib libjpeg lcms harfbuzz] | each {|library| $"--with-($library)=bundled" }
   let system  = [zlib libpng freetype] | each {|library| $"--with-($library)=system" }

   let flags = [
      $"--prefix=($c.out)"
      $"--with-boot-jdk=(bin java | path dirname | path dirname)"
      --with-toolchain-type=clang
      --enable-headless-only
      --disable-warnings-as-errors
      --disable-precompiled-headers
      --with-native-debug-symbols=internal
      --with-stdc++lib=dynamic
      ...$system
      ...$bundled
      $"--with-cups-include=(dep cups-headers cups)/include"
      $"--with-fontconfig-include=(dep fontconfig-headers fontconfig)/include"
      $"--with-jobs=($c.njobs)"
      --with-version-build=1
      --with-version-pre=
      --with-version-opt=pkgs
      --with-vendor-name=pkgs
      # jar --date rejects times before 1980-01-01T00:00:02Z, two seconds past the DOS epoch
      --with-source-date=315532802
      --with-hotspot-build-time=1980-01-01T00:00:02
      --with-build-user=pkgs
   ]

   # configure is a wrapper of its own that wants bash
   cue bash configure ...$cross ...$flags
}

export def build []: nothing -> nothing {
   cue make images $"JOBS=((act).njobs)" LOG=info
}

export def install []: nothing -> nothing {
   let c     = (act)
   let image = (glob-files --any build/*/images/jdk | first)

   mkdir $c.out
   for entry in [bin conf include jmods lib release] {
      cp -r $"($image)/($entry)" $c.out
   }
}
