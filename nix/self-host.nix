{
  system ? builtins.currentSystem,
  seed,
  rustToolchain,
}:
let
  common = {
    inherit system rustToolchain;
    seed = builtins.storePath seed;
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
