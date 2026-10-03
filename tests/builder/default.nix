# Builder behaviour, one test per directory. Its test.nu runs under the bootstrap's nu with builder/
# and lib/ on the include path and $env.fixtures naming the directory. A directory with a
# recipe.toml or package.nix is first built as the out-of-tree package test-<name>, handed over as
# $env.pkg and $env.debug, so each test needs only its own toolchain. `nix-build tests/builder -A
# <name>` runs one and `manicpkgs test` all.
{
  system ? builtins.currentSystem,
  platform ? system,
  crib ? null,
  rustToolchain ? null,
  buildTools ? { },
}:
let
  inherit (builtins)
    attrNames
    filter
    fromJSON
    fromTOML
    listToAttrs
    mapAttrs
    pathExists
    readDir
    readFile
    ;
  here = removeAttrs (readDir ./.) [
    "lib"
    "default.nix"
  ];
  testDir = name: ./. + "/${name}";
  built =
    name:
    let
      manifest = testDir name + "/recipe.toml";
    in
    pathExists (testDir name + "/package.nix")
    || pathExists manifest && fromTOML (readFile manifest) ? build;
  # named test-<name>, since a bare `go` would replace the go that the go build system runs
  set = import ../../nix/set.nix {
    inherit
      system
      platform
      crib
      rustToolchain
      buildTools
      ;
    packages = listToAttrs (
      map (name: {
        name = "test-${name}";
        value = testDir name;
      }) (filter built (attrNames here))
    );
  };
  inherit (set) tools;
  # nu takes include dirs separated by \x1e
  includes = "${../../builder}${fromJSON "\"\\u001e\""}${./lib}";
  check =
    name: _:
    let
      dir = testDir name;
      pkg = set.pkgs."test-${name}";
    in
    derivation (
      {
        name = "check-${name}";
        inherit system;
        __contentAddressed = true;
        outputHashMode = "recursive";
        outputHashAlgo = "sha256";
        builder = "${tools.nu}/bin/nu";
        args = [
          "--no-config-file"
          "--include-path=${includes}"
          "${dir}/test.nu"
        ];
        PATH = "${tools.nu}/bin:${tools.hoist}/bin:${set.bootstrap.crib}/bin";
        fixtures = dir;
        SYS_LIBS = ../../builder/sys-libs.json;
      }
      // (
        if built name then
          {
            inherit pkg;
            inherit (pkg) debug;
            src = "${dir}/src";
          }
        else
          { }
      )
    );
in
mapAttrs check here
