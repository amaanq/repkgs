{ pkgs, ... }:
[ { meson.defs.xkb-config-root = "${pkgs.xkeyboard-config}/share/X11/xkb"; } ]
