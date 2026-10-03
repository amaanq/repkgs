{
  attr,
  system ? builtins.currentSystem,
  platform ? system,
  seed ? null,
  rustToolchain ? null,
  buildTools ? { },
}:
let
  set = import ../../nix/set.nix {
    inherit
      system
      platform
      seed
      rustToolchain
      buildTools
      ;
  };
  package = set.pkgs.${attr} or (throw "no package ${attr}");
in
assert package.supported;
derivation (
  package.drvAttrs
  // {
    name = "${package.name}-dev";
    outputs = [ "out" ];
    args = [
      "dev"
      "export"
    ];
    devOutputs = package.outputs;
    devNu = set.tools.nu;
  }
)
