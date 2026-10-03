# CI for one build machine, `nix-build nix/checks.nix -A pkg-jq` without flakes and `checks` in
# flake.nix. Every supported package is `pkg-<name>` natively and `<platform>-<name>` elsewhere,
# separate test suites are `tests-<name>` and tests/builder is `builder-<name>`. The msvc platform
# stays out of the default list because its toolchain carries the unfree Windows SDK, which the
# public cache must not hold.
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
  host = import nixpkgs { inherit system; };
  inherit (host) lib;

  setFor = platform: import ../default.nix { inherit system platform; };
  prefixed = prefix: lib.mapAttrs' (name: lib.nameValuePair "${prefix}${name}");
  # `supported` is known without forcing the derivation
  supported = lib.filterAttrs (_: package: package.supported);
  here = supported (setFor system);
  packagesFor =
    platform:
    prefixed (if platform == system then "pkg-" else "${platform}-") (supported (setFor platform));
  suites = lib.concatMapAttrs (
    name: package: lib.optionalAttrs (package ? tests) { ${name} = package.tests; }
  ) here;
  checkout = builtins.path {
    path = ../.;
    name = "source";
    filter =
      path: type:
      !(builtins.elem (baseNameOf path) [
        ".jj"
        "target"
      ])
      && lib.cleanSourceFilter path type;
  };
  formatter = import ./format.nix {
    pkgs = host;
    evaluates = false;
  };
in
lib.mergeAttrsList (map packagesFor platforms)
// prefixed "tests-" suites
// prefixed "builder-" (import ../tests/builder { inherit system; })
// {
  schema = host.runCommand "hoist-schema-check" { } ''
    ${(setFor system).hoist}/bin/hoist schema --check ${./schema.json}
    touch $out
  '';
  format = host.runCommand "format-check" {
    nativeBuildInputs = [ formatter ];
  } "cp -r ${checkout} src && chmod -R u+w src && cd src && nuff --check && touch $out";
  inherit
    (import ../pkgs/se/seed {
      inherit system;
      buildSystem = system;
    })
    seed
    ;
}
