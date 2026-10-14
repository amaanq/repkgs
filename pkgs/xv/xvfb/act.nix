{ pkgs, ... }:
[
  {
    meson.defs = {
      xkb_bin_dir = "${pkgs.xkbcomp}/bin";
      xkb_dir = "${pkgs.xkeyboard-config}/share/X11/xkb";
    };
  }
]
