# libc reads time zones from /usr/share/zoneinfo and then /etc/zoneinfo for NixOS, never from its
# own prefix (pkgs/gl/glibc/glibc-tzdir-etc-zoneinfo.patch). The sandbox has neither, so this reads
# the paths compiled into the libc and does one real lookup through TZDIR
use checks.nu [expect pass]
use glob.nu [glob-files]

# the sandbox holds only the closure, so the store's one libc.so.6 is the program's
let libc     = (glob-files /nix/store/*-sysroot-*/lib/libc.so.6 | first)
let compiled = (open --raw $libc | decode utf-8 | split row "\u{0}" | where $it =~ zoneinfo | sort)
expect $"libc looks in the system's zoneinfo directories, not ($compiled)" ($compiled == ["/etc/zoneinfo/%s" "/usr/share/zoneinfo"])
expect "a zone file resolves"                                              ((with-env {TZDIR: $"($env.fixtures)/zoneinfo", TZ: "Test/Zone"} { ^$"($env.pkg)/bin/tz" }) == "TST")
pass
