{ pkgs, ... }:
[ { meson.defs.pci-ids = "${pkgs.hwdata}/share/hwdata"; } ]
