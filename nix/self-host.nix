{
  system ? builtins.currentSystem,
  crib,
  rustToolchain,
}:
let
  common = {
    inherit system rustToolchain;
    crib = builtins.storePath crib;
  };
  initial = import ./set.nix common;
  executors = set: {
    inherit (set.pkgs) shim hoist;
    nu = set.pkgs.nushell;
  };
  first = import ./set.nix (common // { buildTools = executors initial; });
  second = import ./set.nix (common // { buildTools = executors first; });
in
{
  stage1 = executors initial;
  stage2 = executors first;
  inherit (second) pkgs;
}
