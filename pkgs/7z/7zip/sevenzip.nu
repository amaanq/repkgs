use core.nu [act cue place-bins]

export def build []: nothing -> nothing {
   let c      = (act)
   let flavor = ({x86_64: "_x64", aarch64: "_arm64"} | get -o $c.platform.cpu | default "")
   do {
      cd CPP/7zip/Bundles/Alone2
      (cue make $"-j($c.njobs)" -f $"../../cmpl_clang($flavor).mak" CC=cc CXX=c++
         DISABLE_RAR_COMPRESS=true USE_ASM= "CFLAGS_WARN_WALL=-Wall -Wextra" LFLAGS_STRIP=
         "CFLAGS_DEBUG=-g -DNDEBUG" $"O=($c.build)")
   }
   place-bins $c.build [7zz]
}
