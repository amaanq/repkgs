# A development shell with treefmt (treefmt.nix), rom and what uptrack runs, from plain nixpkgs
# (nix/nixpkgs.nix) and apart from the package set
{
  pkgs ? import (import ./nix/nixpkgs.nix) { },
  rom ?
    let
      locked = (builtins.fromJSON (builtins.readFile ./flake.lock)).nodes.rom.locked;
      source = fetchTarball {
        url = "https://github.com/${locked.owner}/${locked.repo}/archive/${locked.rev}.tar.gz";
        sha256 = locked.narHash;
      };
    in
    pkgs.callPackage (source + "/nix/package.nix") { },
}:
let
  host = pkgs.stdenv.hostPlatform;
  wild = host.isLinux && (host.isx86_64 || host.isAarch64);
in
pkgs.mkShell {
  packages = [
    rom
    (import ./treefmt.nix { inherit pkgs; })
    (import ./nix/uptrack.nix { inherit pkgs; })
    # bsdtar, so uptrack's prefetch unpacks the way nix/sources.nix does
    pkgs.libarchive
    # uptrack formats package.toml the way treefmt does
    pkgs.taplo
  ]
  ++ pkgs.lib.optionals wild [
    pkgs.wild
    pkgs.clang
  ];
}
