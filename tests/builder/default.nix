# Builder behaviour, one test per directory. Its test.nu runs under the bootstrap's nu with builder/
# and lib/ on the include path and $env.fixtures naming the directory. A directory with a
# act.toml or package.nix is first built as the out-of-tree package test-<name>, handed over as
# $env.pkg and $env.debug, so each test needs only its own toolchain. `nix-build tests/builder -A
# <name>` runs one and `motley test` all.
{
  system ? builtins.currentSystem,
  platform ? system,
  peanut ? null,
  rustToolchain ? null,
  buildTools ? { },
}:
let
  inherit (builtins)
    attrNames
    filter
    fromJSON
    listToAttrs
    mapAttrs
    pathExists
    readDir
    ;
  here = removeAttrs (readDir ./.) [
    "lib"
    "default.nix"
  ];
  testDir = name: ./. + "/${name}";
  built =
    name:
    pathExists (testDir name + "/package.nix")
    || pathExists (testDir name + "/act.toml") && (import ../../nix/act.nix (testDir name)).isPackage;
  # named test-<name>, since a bare `go` would replace the go that the go build system runs
  set = import ../../nix/set.nix {
    inherit
      system
      platform
      peanut
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
        PATH = "${tools.nu}/bin:${tools.bigtop}/bin:${set.bootstrap.peanut}/bin";
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
