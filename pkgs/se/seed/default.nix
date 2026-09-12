{
  system ? builtins.currentSystem,
  buildSystem ? builtins.currentSystem,
}:
let
  pin = (builtins.fromJSON (builtins.readFile ../../../flake.lock)).nodes.nixpkgs.locked;
  nixpkgs = builtins.fetchTarball {
    url = "https://github.com/${pin.owner}/${pin.repo}/archive/${pin.rev}.tar.gz";
    sha256 = pin.narHash;
  };
in
import ./build.nix { inherit nixpkgs system buildSystem; }
