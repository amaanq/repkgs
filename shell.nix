# Development shell: `treefmt` (treefmt.nix) and what uptrack shells out to. Not part of the
# package set. Plain nixpkgs (nix/nixpkgs.nix).
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
  hasWild =
    pkgs.stdenv.hostPlatform.isLinux
    && (pkgs.stdenv.hostPlatform.isx86_64 || pkgs.stdenv.hostPlatform.isAarch64);
in
pkgs.mkShell {
  packages = [
    rom
    (import ./treefmt.nix { inherit pkgs; })
    pkgs.libarchive # bsdtar, uptrack's prefetch unpacks like nix/sources.nix does
    pkgs.taplo # uptrack writes sources.toml the way treefmt formats it
  ]
  ++ pkgs.lib.optionals hasWild [
    pkgs.wild
    pkgs.clang
  ];
}
