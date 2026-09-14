# bin/foo becomes a symlink to the `launch` binary (pkgs/la/launch), the real file moves to
# bin/.foo, and bin/.foo.launch is a JSON record saying what to exec and with which environment.
# Used for every script (the record names its interpreter), and for programs too when a
# dependency contributes PATH entries or env defaults.
#
# `prebuilt = "ldso"` packages (rust-bootstrap: formatelf is built with it) also go through
# here: the upstream ELF stays untouched and its launcher runs it as
#   <sysroot>/lib/ld.so --argv0 bin/foo --library-path <libc and deps> bin/.foo
# argv[0] still says bin/foo, which rustc needs to find its sysroot.
#
# PE has no RUNPATH and no dependable symlinks: bin/foo.exe is a *copy* of launch.exe, the record's
# PATH names the store dirs holding its DLLs (also the reference Nix sees).

use core.nu *

export def main [c: record]: nothing -> nothing {
  let ldso = (if ($c.spec.prebuilt? | default false) == "ldso" { [--ldso] } else { [] })
  ^jib launchers $c.out --launch $c.platform.launch --interp $c.platform.interp ...$ldso ...(attrs).dependencies
}
