use checks.nu *

let lib = $"($env.pkg)/lib"
split-debug bin/hello
split-debug lib/libgreet.so.1.2
expect "libraries land in lib/ and not lib64/"              ($"($lib)/libgreet.so" | path exists)
expect "the soname link is relative"                        ((^readlink $"($lib)/libgreet.so.1") == "libgreet.so.1.2")
expect "the exported config names libm without the sysroot" ((open --raw $"($lib)/cmake/greet/greetConfig.cmake") !~ 'sysroot-[^"]*/libm\.')

let exports = (open $"($env.pkg)/exports.json")
expect "exports.json lists include/"      ("include" in $exports.includeDirs)
expect "exports.json lists lib/pkgconfig" ("lib/pkgconfig" in $exports.pkgconfigDirs)
expect "exports.json lists libgreet"      ("greet" in $exports.libs)
pass
