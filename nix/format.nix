# `nix fmt`, `motley fmt` or `nuff` in the development shell formats the tree and `nuff --check`
# checks it. nuff reads ../nuff.toml, and this wrapper pins nuff and every tool that config names.
{
  pkgs,
  # false inside a build (nix/checks.nix), where the package-scripts lint has no evaluator to
  # instantiate the set with
  evaluates ? true,
}:
let
  inherit (pkgs) lib;
  nuff = pkgs.callPackage "${
    builtins.fetchGit {
      url = "https://github.com/manic-systems/nuff";
      rev = "a99fadda6130d62f92e87963218740c2455a2c70";
    }
  }/nix/package.nix" { };
  fenix = (builtins.fromJSON (builtins.readFile ../flake.lock)).nodes.fenix.locked;
  rustfmt =
    (import (builtins.fetchTarball {
      url = "https://github.com/${fenix.owner}/${fenix.repo}/archive/${fenix.rev}.tar.gz";
      sha256 = fenix.narHash;
    }) { inherit pkgs; }).latest.rustfmt;
  tools = [
    pkgs.nushell
    pkgs.nixfmt
    pkgs.statix
    pkgs.deadnix
    pkgs.go
    rustfmt
    pkgs.llvmPackages_23.clang-tools
    pkgs.ruff
    pkgs.taplo
    pkgs.ast-grep
    (import ./spieler.nix { inherit pkgs; })
  ];
in
pkgs.writeShellScriptBin "nuff" ''
  export PATH=${lib.makeBinPath tools}:$PATH
  # ast-grep reads nu through the tree-sitter grammar nushell maintains
  export TREE_SITTER_NU=${pkgs.tree-sitter-grammars.tree-sitter-nu}/parser
  ${lib.optionalString (!evaluates) "export MOTLEY_NO_EVAL=1"}
  exec ${lib.getExe nuff} "$@"
''
