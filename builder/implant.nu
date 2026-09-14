# `prebuilt = true`: upstream ELF binaries get our dynamic linker, the relocation stub and a
# RUNPATH written into them by formatelf, then pass through reloc-fixup like our own builds.

use core.nu *

# Every dynamically linked ELF under bin/, lib/, libexec/ of `dir` (default $out) gets what our
# linker would have given it:
#   - a RUNPATH over libc and the dependencies' lib dirs, padded so reloc-fixup can rewrite it
#     in place afterwards, with the same slack the driver gives its own links
#   - for executables also our ld.so as interpreter and the relocation stub as entry point
# The binary keeps a real /proc/self/exe this way, which bun and node need to re-exec.
#
# `dir` is for bindists whose install step runs the binaries from the unpacked tree before they
# reach $out (ghc's `make install` calls the just-installed ghc-pkg). That tree gets interpreter
# and RUNPATH but no stub, since the stub only works after reloc-fixup, which runs over $out.
# Idempotent: a second pass over the same files writes the same strings into the same place.
export def main [c: record, dir?: path]: nothing -> nothing {
  let dir = ($dir | default $c.out)
  let stub = (if $dir == $c.out { [--stub $c.platform.relocStub] } else { [] })
  ^jib implant $dir --interp $c.platform.interp ...$stub ...(attrs).dependencies
}
