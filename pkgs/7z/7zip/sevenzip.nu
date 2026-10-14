use core.nu [ctx cue install-bins]

export def build []: nothing -> nothing {
   let c      = (ctx)
   let flavor = ({x86_64: "_x64", aarch64: "_arm64"} | get -o $c.platform.cpu | default "")
   do {
      cd CPP/7zip/Bundles/Alone2
      (cue make $"-j($c.njobs)" -f $"../../cmpl_clang($flavor).mak" CC=cc CXX=c++
         DISABLE_RAR_COMPRESS=true USE_ASM= "CFLAGS_WARN_WALL=-Wall -Wextra" LFLAGS_STRIP=
         "CFLAGS_DEBUG=-g -DNDEBUG" $"O=($c.build)")
   }
   install-bins $c.build [7zz]
}
