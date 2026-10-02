# `nix fmt` or `nix-shell --run treefmt` formats and `treefmt --ci` checks. nixpkgs'
# treefmt.withConfig pins every tool here, so shell.nix carries only the wrapper.
{
  pkgs,
  # false inside a build (nix/checks.nix), where package-scripts has no evaluator to instantiate
  # the set with
  evaluates ? true,
}:
let
  inherit (pkgs) lib;
  nu = "${pkgs.nushell}/bin/nu";
  fenix = (builtins.fromJSON (builtins.readFile ./flake.lock)).nodes.fenix.locked;
  rustfmt =
    (import (builtins.fetchTarball {
      url = "https://github.com/${fenix.owner}/${fenix.repo}/archive/${fenix.rev}.tar.gz";
      sha256 = fenix.narHash;
    }) { inherit pkgs; }).latest.rustfmt;
  # statix takes one path per run
  statixEach = pkgs.writeShellScript "statix-each" ''
    status=0
    for file in "$@"; do ${pkgs.statix}/bin/statix check -c ${./statix.toml} "$file" || status=1; done
    exit $status
  '';
  # ast-grep reads nu through the tree-sitter grammar nushell maintains
  astGrep = pkgs.writeText "sgconfig.yml" (
    builtins.toJSON {
      ruleDirs = [ "${./lints/nu}" ];
      customLanguages.nu = {
        libraryPath = "${pkgs.tree-sitter-grammars.tree-sitter-nu}/parser";
        extensions = [ "nu" ];
        expandoChar = "_";
      };
    }
  );
  nuFiles = [
    "*.nu"
    "tools/repkgs"
  ];

in
pkgs.treefmt.withConfig {
  settings = {
    global.excludes = [
      "flake.lock"
      "pkgs/ll/llvm/*.txt"
      "result*"
      "**/lock.json"
      "**/target/**"
    ];
    formatter = {
      nix = {
        command = "${pkgs.nixfmt}/bin/nixfmt";
        includes = [ "*.nix" ];
      };
      nix-statix = {
        command = "${statixEach}";
        includes = [ "*.nix" ];
      };
      nix-deadnix = {
        command = "${pkgs.deadnix}/bin/deadnix";
        options = [
          "--fail"
          "--edit"
        ];
        includes = [ "*.nix" ];
      };
      go = {
        command = "${pkgs.go}/bin/gofmt";
        options = [ "-w" ];
        includes = [ "pkgs/*/*/src/*.go" ];
      };
      rust = {
        command = "${rustfmt}/bin/rustfmt";
        options = [
          "--edition"
          "2024"
          "--config"
          "skip_children=true"
        ];
        includes = [ "pkgs/*/*/src/*.rs" ];
      };
      c = {
        command = "${pkgs.llvmPackages_23.clang-tools}/bin/clang-format";
        options = [ "-i" ];
        includes = [
          "pkgs/*/*/src/*.c"
          "pkgs/*/*/src/*.h"
        ];
      };
      python = {
        command = "${pkgs.ruff}/bin/ruff";
        options = [ "format" ];
        includes = [ "*.py" ];
      };
      python-lint = {
        command = "${pkgs.ruff}/bin/ruff";
        options = [
          "check"
          "--fix"
        ];
        includes = [ "*.py" ];
      };
      toml = {
        command = "${pkgs.taplo}/bin/taplo";
        options = [ "format" ];
        includes = [ "*.toml" ];
        excludes = [ "locks/*.toml" ];
      };
      # one sorted entry per line, so additions merge as text (.gitattributes merge=union)
      locks = {
        command = "${import ./nix/uptrack.nix { inherit pkgs; }}/bin/uptrack";
        options = [ "normalize" ];
        includes = [ "locks/*.toml" ];
      };
      # nu has no stable formatter, so its files are only parsed and type checked
      nu-typecheck = {
        command = nu;
        options = [
          "--no-config-file"
          "lints/nu-typecheck.nu"
        ];
        includes = nuFiles;
      };
      package-scripts = lib.mkIf evaluates {
        command = nu;
        options = [
          "--no-config-file"
          "lints/package-scripts.nu"
        ];
        includes = [
          "pkgs/*/*/package.toml"
          "pkgs/*/*/*.nu"
        ];
      };
      nu-ast-grep = {
        command = "${pkgs.ast-grep}/bin/ast-grep";
        options = [
          "scan"
          "--config=${astGrep}"
          "--report-style=short"
        ];
        includes = nuFiles;
      };
    };
  };
}
