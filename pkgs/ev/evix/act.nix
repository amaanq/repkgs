{ file, ... }:
[ { cargo.cratePatches.nix-bindings-sys = [ (file "nix-bindings-sys-fromattrs.patch") ]; } ]
