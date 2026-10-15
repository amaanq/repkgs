{ toolchain, ... }:
[ { source = builtins.head toolchain.sysroot.parts; } ]
