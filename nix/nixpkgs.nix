# flake.lock's nixpkgs for the entry points that run without flakes
let
  pin = (builtins.fromJSON (builtins.readFile ../flake.lock)).nodes.nixpkgs.locked;
in
fetchTarball {
  url = "https://github.com/NixOS/nixpkgs/archive/${pin.rev}.tar.gz";
  sha256 = pin.narHash;
}
