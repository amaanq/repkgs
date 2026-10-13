{ pkgs, ... }:
[
  {
    autotools.flags = [ "--with-libgpg-error-prefix=${pkgs.libgpg-error}" ];
  }
]
