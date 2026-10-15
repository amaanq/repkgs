# What CI builds on one machine, also the flake's `checks`. Run one with
# `nix-build nix/checks.nix -A pkg-jq`. msvc is left out by default since its toolchain carries the
# unfree Windows SDK, which must not reach the public cache.
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

  rename = f: lib.mapAttrs' (name: value: lib.nameValuePair (f name) value);
  setFor = platform: import ../default.nix { inherit system platform; };
  usable = platform: lib.filterAttrs (_: p: p.supported) (setFor platform);
  native = usable system;

  perPlatform = map (
    platform:
    rename (name: if platform == system then "pkg-${name}" else "${platform}-${name}") (usable platform)
  ) platforms;
  testSuites = rename (name: "tests-${name}") (
    lib.mapAttrs (_: p: p.tests) (lib.filterAttrs (_: p: p ? tests && p.tests.supported or true) native)
  );
  builderTests = rename (name: "builder-${name}") (import ../tests/builder { inherit system; });

  tree = builtins.path {
    name = "source";
    path = ../.;
    filter =
      path: type:
      !lib.elem (baseNameOf path) [
        ".jj"
        "target"
      ]
      && lib.cleanSourceFilter path type;
  };
in
lib.foldl' lib.mergeAttrs { } perPlatform
// testSuites
// builderTests
// {
  schema = pkgs.runCommand "bigtop-schema-check" { } ''
    ${(setFor system).bigtop}/bin/bigtop schema --check ${./schema.json}
    touch $out
  '';
  format = pkgs.runCommand "format-check" {
    nativeBuildInputs = [
      (import ./format.nix {
        inherit pkgs;
        evaluates = false;
      })
    ];
  } "cp -r ${tree} src && chmod -R u+w src && cd src && nuff --check && touch $out";
  inherit
    (import ../pkgs/pe/peanut {
      inherit system;
      buildSystem = system;
    })
    peanut
    ;
}
