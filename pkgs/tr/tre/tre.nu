use core.nu *

# the tests want an ISO-8859-1 locale
export def --env latin1 []: nothing -> nothing {
   test-locale en_US ISO-8859-1
}

# mingw has no regex(3): libregex and libgnurx are libtre plus the four POSIX entry points
export def posix-regex []: nothing -> nothing {
   let c = (ctx)
   "#include <tre/regex.h>\n" | save $"($c.out)/include/regex.h"
   x cc -O2 $"-I($c.out)/include" -c $env.SYSTRE -o systre.o
   for name in [regex gnurx] {
      let archive = $"($c.out)/lib/lib($name).a"
      cp $"($c.out)/lib/libtre.a" $archive
      x llvm-ar q $archive systre.o
   }
}
