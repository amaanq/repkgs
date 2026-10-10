{ file, ... }:
[
  { go.modulePatches."github.com/rogpeppe/go-internal" = [ (file "testscript-drain-pty.patch") ]; }
]
