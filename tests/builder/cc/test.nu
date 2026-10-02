use checks.nu *

let lib = $"($env.pkg)/lib"
split-debug bin/hello
split-debug lib/libhello.so
expect "the static archive keeps no DWARF"                     (not (section-in lib/libhello.a .debug_info))
expect "an upstream ELF keeps its DWARF"                       (section-in lib/upstream.so .debug_info)
expect "an install entry copies a whole directory"             ($"($env.pkg)/share/data/file" | path exists)
expect "a symlink into the output becomes relative"            ((^readlink $"($lib)/libabs.so") == "libhello.so")
expect "a library copied from a dependency is not split again" (section-in lib/libcopied.so .gnu_debuglink)
expect "a library copied from a dependency still finds libc"   (^readelf -d $"($lib)/libcopied.so" | str contains "sysroot")
expect "libtool archives are deleted"                          (not ($"($lib)/libhello.la" | path exists))
pass
