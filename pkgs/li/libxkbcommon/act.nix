{ pkgs, ... }:
[ { meson.defs.x-locale-root = "${pkgs.libx11}/share/X11/locale"; } ]
