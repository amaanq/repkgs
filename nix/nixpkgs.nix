# flake.lock's nixpkgs for the entry points without flakes (shell.nix, nix/checks.nix and
# pkgs/pe/peanut/build.nix), so their tools and the peanut's nu are the ones the tree expects
let
  inherit ((builtins.fromJSON (builtins.readFile ../flake.lock)).nodes.nixpkgs) locked;
in
fetchTarball {
  url = "https://github.com/NixOS/nixpkgs/archive/${locked.rev}.tar.gz";
  sha256 = locked.narHash;
}
