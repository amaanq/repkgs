# Nix's development branch plus three fixes the bootstrap leans on, NixOS/nix#16459 (dynamic
# derivations rebuild after a GC), #16465 (CA rebuilds of derivations that see their own output path)
# and #16477 (a CA rebuild that differs from a collected output replaces its build trace).
{
  pkgs ? import (import ../nixpkgs.nix) { },
}:
pkgs.nixVersions.git.overrideSource (
  pkgs.fetchFromGitHub {
    owner = "Mic92";
    repo = "nix-1";
    rev = "e6c24e710343d0485995d1a67c6f4971d698bbca";
    hash = "sha256-XVtq1d7UwIj4/J3g1uaQkEq6bdu0p85JHZEFlm9qukg=";
  }
)
