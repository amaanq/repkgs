{
  pkgs ? import (import ./nix/nixpkgs.nix) { },
  rom ?
    let
      pin = (builtins.fromJSON (builtins.readFile ./flake.lock)).nodes.rom.locked;
      src = builtins.fetchTarball {
        url = "https://github.com/${pin.owner}/${pin.repo}/archive/${pin.rev}.tar.gz";
        sha256 = pin.narHash;
      };
    in
    pkgs.callPackage "${src}/nix/package.nix" { },
}:
let
  host = pkgs.stdenv.hostPlatform;
in
pkgs.mkShell {
  packages = [
    rom
    (import ./nix/format.nix { inherit pkgs; })
    (import ./nix/spieler.nix { inherit pkgs; })
    # spieler prefetches with bsdtar so archives unpack as nix/sources.nix unpacks them, and
    # formats act.lock with taplo as nuff does
    pkgs.libarchive
    pkgs.taplo
  ]
  ++ pkgs.lib.optionals (host.isLinux && (host.isx86_64 || host.isAarch64)) [
    pkgs.wild
    pkgs.clang
  ];
}
