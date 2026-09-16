# `nix fmt` or `nix-shell --run treefmt` formats, `treefmt --ci` is the check. Built with nixpkgs'
# `treefmt.withConfig` so every tool is pinned here and shell.nix only carries the wrapper.
{
  pkgs,
  # nix/checks.nix: inside a build there is no evaluator for package-scripts (it instantiates the set)
  evaluates ? true,
}:
let
  llvm = pkgs.llvmPackages_23;
  fenix = (builtins.fromJSON (builtins.readFile ./flake.lock)).nodes.fenix.locked;
  rustfmt =
    (import (builtins.fetchTarball {
      url = "https://github.com/${fenix.owner}/${fenix.repo}/archive/${fenix.rev}.tar.gz";
      sha256 = fenix.narHash;
    }) { inherit pkgs; }).latest.rustfmt;
  # nu's own parser/type checker over each file (authoritative). Fails on any error diagnostic.
  # cell-path-types: `$rec.field` takes the field's declared type instead of `any`, so typed
  # records are checked where they are used, not just where they are built
  nu-typecheck = pkgs.writeShellScript "nu-typecheck" ''
    status=0
    for f in "$@"; do
      out=$(${pkgs.nushell}/bin/nu --no-config-file --include-path "$PWD/builder:$PWD/tests/builder/lib" "--experimental-options=[cell-path-types]" --ide-check 50 "$f" | ${pkgs.jq}/bin/jq -r 'select(.type == "diagnostic" and .severity == "Error") | "\(.span.start): \(.message)"')
      if [ -n "$out" ]; then printf '%s:\n%s\n' "$f" "$out" >&2; status=1; fi
    done
    exit $status
  '';
  # statix checks one path per call
  statix-each = pkgs.writeShellScript "statix-each" ''
    status=0
    for f in "$@"; do ${pkgs.statix}/bin/statix check -c ${./statix.toml} "$f" || status=1; done
    exit $status
  '';
  # ast-grep: nu via the tree-sitter grammar nushell maintains (lints/nu/), nix is built in (lints/nix/)
  sgconfig = pkgs.writeText "sgconfig.yml" (
    builtins.toJSON {
      ruleDirs = [
        "${./lints/nu}"
        "${./lints/nix}"
      ];
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
  uptrack = import ./nix/uptrack.nix { inherit pkgs; };
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
        command = "${statix-each}";
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
      cpp = {
        command = "${llvm.clang-tools}/bin/clang-format";
        options = [ "-i" ];
        includes = [
          "pkgs/*/*/src/*.cc"
          "pkgs/*/*/src/*.h"
          "pkgs/*/*/src/*.c"
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
      # one entry per line, sorted, so additions merge textually (.gitattributes merge=union)
      locks = {
        command = "${uptrack}/bin/uptrack";
        options = [ "normalize" ];
        includes = [ "locks/*.toml" ];
      };
      # nu has no stable formatter, this only checks (parse + types with the nu that runs builds).
      nu-typecheck = {
        command = "${nu-typecheck}";
        includes = nuFiles;
      };
      # the nu that package.nix assembles from package.nix, its modules and the build systems
      package-scripts = pkgs.lib.mkIf evaluates {
        command = "${pkgs.nushell}/bin/nu";
        options = [
          "--no-config-file"
          "lints/package-scripts.nu"
        ];
        includes = [
          "pkgs/*/*/package.nix"
        ];
      };
      nu-ast-grep = {
        command = "${pkgs.ast-grep}/bin/ast-grep";
        options = [
          "scan"
          "--config=${sgconfig}"
          "--report-style=short"
        ];
        includes = nuFiles ++ [ "pkgs/*/*/*.nix" ];
      };
    };
  };
}
