# Everything after the last phase, `jib finish`, plus the steps bench/ measures on their own
use core.nu *

# --env: the cd must outlive the call. Globs in spec.install are relative to the last phase's
# directory, which is where jib finish runs
export def --env main [
  --keep-tree  # tests.separate: save source+build tree for the tests derivation
]: nothing -> nothing {
  ^jib finish ...(if $keep_tree { [--keep-tree] } else { [] })
  cd $env.NIX_BUILD_TOP
}

# tests derivation output: a result marker. The package itself is untouched
export def tests []: nothing -> nothing {
  ^jib finish --tests
}

# docs, junk with absolute paths or timestamps removed. Returns the inventory of what is left
export def prune [out: path]: nothing -> table {
  ^jib prune $out | from json
}

# DWARF -> `debug` under lib/debug/.build-id/, .symtab and a .gnu_debuglink stay. Only for what
# jig linked (its package note): no DWARF and no debuglink there means the build strips, an error.
# Upstream binaries and ours copied from a dependency (already split) are left alone
export def split-debug [out: path, debug: path]: nothing -> nothing {
  ^jib split-debug $out $debug
}

# lib/ and bin/ only
export def layout-check [out: path]: nothing -> nothing {
  for d in [lib64 sbin] {
    if ($"($out)/($d)" | path exists) { error make {msg: $"($d)/ in output: configure with --libdir/--sbindir so it installs into lib/ and bin/"} }
  }
}
