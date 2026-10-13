{ pkgs, ... }:
[
  {
    autotools.flags = [
      "--with-libgpg-error-prefix=${pkgs.libgpg-error}"
      "--with-libgcrypt-prefix=${pkgs.libgcrypt}"
      "--with-libassuan-prefix=${pkgs.libassuan}"
      "--with-ksba-prefix=${pkgs.libksba}"
      "--with-npth-prefix=${pkgs.npth}"
    ];
  }
]
