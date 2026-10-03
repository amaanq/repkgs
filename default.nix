# One platform's packages, `nix-build -A zlib`, or `--argstr platform riscv64-linux` to target
# another with the tools still built here. `overrides` edits recipes (nix/overrides.nix),
# `packages` adds or replaces directories by name, and `features` sets values for every package
# declaring them (nix/features.nix).
{
  system ? builtins.currentSystem,
  platform ? system,
  peanut ? null,
  rustToolchain ? null,
  buildTools ? { },
  overrides ? { },
  packages ? { },
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
