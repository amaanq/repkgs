{
  system ? builtins.currentSystem,
  peanut,
  rustToolchain,
}:
let
  common = {
    inherit system rustToolchain;
    peanut = builtins.storePath peanut;
  };
  initial = import ./set.nix common;
  executors = set: {
    inherit (set.pkgs) mummer bigtop;
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
