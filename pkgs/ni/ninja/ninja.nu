use core.nu [act cue glob-files]

# configure.py would need python, which needs zlib, which builds with cmake and so with ninja
export def build []: nothing -> nothing {
   let c       = (act)
   let windows = $c.platform.os == "windows"
   let skip    = [*_test.cc *_perftest.cc *_bench.cc *.in.cc test.cc browse.cc (if $windows { "*-posix.cc" } else { "*-win32.cc" })]
   let sources = (glob-files $"($c.src)/src/*.cc" --exclude $skip)
   let getopt  = if $windows { [$"($c.src)/src/getopt.c"] } else { [] }
   let flags   = [-O2 -DNDEBUG -fvisibility=hidden -Wno-deprecated '-DNINJA_PYTHON="python3"' ...(if $windows { [-DNOMINMAX -D_CRT_SECURE_NO_WARNINGS] } else { [] })]
   let object  = {|source| $"($source | path parse | get stem).o" }

   cd $c.build
   $sources | par-each {|source| cue c++ ...$flags -c $source -o (do $object $source) } | ignore
   for source in $getopt { cue cc ...$flags -c $source -o (do $object $source) }
   cue c++ ...($sources ++ $getopt | each $object) -o $"ninja($c.platform.ext.exe)"
}
