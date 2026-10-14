{ pkgs, ... }:
[ { env.XKBCOMMON_LIB_PATH = "${pkgs.libxkbcommon-bare}/lib/libxkbcommon.so"; } ]
