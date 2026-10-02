# Nix master with the fixes this set relies on, built through nixpkgs' component packaging from
# the `repkgs` branch of github.com/Mic92/nix-1:
#   https://github.com/NixOS/nix/pull/16459  needed, dynamic derivations rebuild after a gc
#   https://github.com/NixOS/nix/pull/16465  CA rebuilds of derivations that see their $out
#                                            (bootstrap/) no longer conflict
#   https://github.com/NixOS/nix/pull/16477  a CA rebuild that differs from a collected output
#                                            replaces its build trace rather than failing
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
