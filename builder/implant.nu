# What `prebuilt = true` does to upstream ELF binaries before reloc-fixup sees them like our own:
# hoist implant gives every dynamically linked ELF under bin/, lib/ and libexec/ a RUNPATH over libc
# and the dependencies, padded with the slack our driver leaves, and gives executables our ld.so
# and the relocation stub. bun and node re-exec through /proc/self/exe, which stays real this way.
#
# A bindist that runs its binaries from the unpacked tree during install (ghc's `make install`
# runs the new ghc-pkg) passes that tree as `dir`. It gets the interpreter and RUNPATH but no stub,
# which works only once reloc-fixup has been over $out. A second pass writes the same bytes.
use core.nu *

export def main [context: record, dir?: path]: nothing -> nothing {
  let target = ($dir | default $context.out)
  let stub   = if $target == $context.out { [--stub $context.platform.relocStub] } else { [] }
  ^hoist implant $target --interp $context.platform.interp ...$stub ...(attrs).dependencies
}
