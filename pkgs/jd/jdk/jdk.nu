use core.nu *

export def configure []: nothing -> nothing {
  let c = (ctx)
  # jdk.jpackage uses std::nothrow without including <new>, which libstdc++ and older libc++
  # pulled in through <string>. tstrings.h is included by all of it
  let header = "src/jdk.jpackage/share/native/common/tstrings.h"
  open --raw $header | str replace "#include <string>" "#include <new>\n#include <string>" | save -f $header

  let cross = if $c.platform.cross {
    [$"--openjdk-target=($c.platform.gnuTriple)" $"--with-build-jdk=(tool-root jdk)"
       $"BUILD_CC=($env.CC_FOR_BUILD)" $"BUILD_CXX=($env.CXX_FOR_BUILD)"]
  } else { [] }
  let bundled = [giflib libjpeg lcms harfbuzz] | each {|library| $"--with-($library)=bundled" }
  let system = [zlib libpng freetype] | each {|library| $"--with-($library)=system" }
  # jar --date rejects times before 1980-01-01T00:00:02Z, two seconds past the DOS epoch
  let reproducible = [--with-source-date=315532802 --with-hotspot-build-time=1980-01-01T00:00:02 --with-build-user=pkgs]
  # configure is a wrapper of its own that wants bash
  (x bash configure ...$cross $"--prefix=($c.out)"
    $"--with-boot-jdk=(tool java | path dirname | path dirname)"
    --with-toolchain-type=clang --enable-headless-only --disable-warnings-as-errors
    --disable-precompiled-headers --with-native-debug-symbols=internal --with-stdc++lib=dynamic
    ...$system ...$bundled
    $"--with-cups-include=(dep-root cups-headers cups)/include"
    $"--with-fontconfig-include=(dep-root fontconfig-headers fontconfig)/include"
    $"--with-jobs=($c.njobs)"
    --with-version-build=1 --with-version-pre= --with-version-opt=pkgs --with-vendor-name=pkgs
    ...$reproducible)
}

export def build []: nothing -> nothing {
  x make images $"JOBS=((ctx).njobs)" LOG=info
}

export def install []: nothing -> nothing {
  let c = (ctx)
  let image = (glob build/*/images/jdk | first)
  mkdir $c.out
  for entry in [bin conf include jmods lib release] { cp -r $"($image)/($entry)" $c.out }
}
