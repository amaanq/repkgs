# One platform's package set, from pkgs/<xx>/<name>/recipe.toml:
#   nix-build -A zlib                                  for this machine
#   nix-build -A zlib --argstr platform riscv64-linux  for another, tools still built here
{
  system ? builtins.currentSystem,
  platform ? system,
  peanut ? null,
  rustToolchain ? null,
  buildTools ? { },
  # a tree or a list of trees, { zlib.autotools.flags.append = [ … ]; } (nix/overrides.nix)
  overrides ? { },
  # name -> directory with a recipe.toml or package.nix, replacing an in-tree name it shares
  packages ? { },
  # feature values for every package declaring the name (nix/features.nix)
  features ? { },
}:
(import ./nix/set.nix {
  inherit
    system
    platform
    peanut
    rustToolchain
    buildTools
    overrides
    packages
    features
    ;
}).pkgs
