# What CI builds for one build machine: every supported package of the set as `pkg-<name>`
# and per other platform as `<platform>-<name>`, separate test suites (tests.separate) as
# `tests-<name>`, tests/builder as `builder-*`, treefmt and the
# seed. x86_64-windows-msvc is not in the default list: the SDK under its toolchain is unfree
# and stays out of the public cache. flake.nix maps this over its systems as `checks`;
# `nix-build nix/checks.nix` without flakes, `-A pkg-jq` for one.
{
  system ? builtins.currentSystem,
  nixpkgs ? import ./nixpkgs.nix,
  platforms ? [
    "x86_64-linux"
    "aarch64-linux"
    "riscv64-linux"
    "loongarch64-linux"
    "powerpc64le-linux"
    "x86_64-windows-gnu"
  ],
}:
let
  pkgs = import nixpkgs { inherit system; };
  inherit (pkgs) lib;

  setOf = platform: import ../default.nix { inherit system platform; };
  # nix/package decides `supported` per platform without forcing the derivation
  buildable = lib.filterAttrs (_: package: package.supported);
  named = prefix: lib.mapAttrs' (name: value: lib.nameValuePair "${prefix}${name}" value);
  native = buildable (setOf system);
  perPlatform =
    platform:
    named (if platform == system then "pkg-" else "${platform}-") (buildable (setOf platform));
  suites = lib.concatMapAttrs (
    name: package: lib.optionalAttrs (package ? tests) { ${name} = package.tests; }
  ) native;
  tree = builtins.path {
    path = ../.;
    name = "source";
    filter = path: type: baseNameOf path != ".jj" && lib.cleanSourceFilter path type;
  };
in
lib.mergeAttrsList (map perPlatform platforms)
// named "tests-" suites
// named "builder-" (import ../tests/builder { inherit system; })
// {
  schema = pkgs.runCommand "jib-schema-check" { } ''
    ${(setOf system).jib}/bin/jib schema --check ${./schema.json}
    touch $out
  '';
  treefmt = pkgs.runCommand "treefmt-check" {
    nativeBuildInputs = [
      (import ../treefmt.nix {
        inherit pkgs;
        evaluates = false;
      })
    ];
  } "cp -r ${tree} src && chmod -R u+w src && cd src && treefmt --ci --tree-root . && touch $out";
  inherit
    (import ../pkgs/se/seed {
      inherit system;
      buildSystem = system;
    })
    seed
    ;
}
