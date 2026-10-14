use core.nu [act cue test-locale]

# the tests want an ISO-8859-1 locale
export def --env latin1 []: nothing -> nothing {
   test-locale en_US ISO-8859-1
}

# mingw has no regex(3): libregex and libgnurx are libtre plus the four POSIX entry points
export def posix-regex []: nothing -> nothing {
   let c = (act)
   "#include <tre/regex.h>\n" | save $"($c.out)/include/regex.h"
   cue cc -O2 $"-I($c.out)/include" -c $env.SYSTRE -o systre.o
   for name in [regex gnurx] {
      let archive = $"($c.out)/lib/lib($name).a"
      cp $"($c.out)/lib/libtre.a" $archive
      cue llvm-ar q $archive systre.o
   }
}
