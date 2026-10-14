use core.nu [act cue]

export def build []: nothing -> nothing {
   let c   = (act)
   let lib = $"($c.out)/lib"
   mkdir $lib
   (cue cc -shared -O2 -o $"($lib)/libgcc_s.so.1" $"($c.src)/gcc_s.c"
      $"-Wl,--version-script=($c.src)/gcc_s.ver" -Wl,-soname,libgcc_s.so.1
      -Wl,--whole-archive $"($c.platform.sysroot)/lib/libunwind.a" -Wl,--no-whole-archive
      -nostdlib++)
}
