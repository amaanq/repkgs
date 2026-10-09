# One check per directory, `nix-build tests/builder -A cmake`. test.nu runs under the bootstrap nu
# with builder/ and lib/ on its include path and `fixtures` set to its directory. A directory that
# describes a package is first built as `test-<name>` and handed over as `pkg` and `debug`.
{
  system ? builtins.currentSystem,
  platform ? system,
  peanut ? null,
  rustToolchain ? null,
  buildTools ? { },
}:
let
  names = builtins.filter (name: name != "lib" && name != "default.nix") (
    builtins.attrNames (builtins.readDir ./.)
  );
  pathOf = name: ./. + "/${name}";
  builds =
    name:
    let
      dir = pathOf name;
    in
    builtins.pathExists (dir + "/package.nix")
    || builtins.pathExists (dir + "/act.toml") && (import ../../nix/act.nix dir).isPackage;
  built = builtins.filter builds names;

  # prefixed so `go` does not replace the in-tree go the go build system uses
  set = import ../../nix/set.nix {
    inherit
      system
      platform
      peanut
      rustToolchain
      buildTools
      ;
    packages = builtins.listToAttrs (
      map (name: {
        name = "test-${name}";
        value = pathOf name;
      }) built
    );
  };
  inherit (set.tools) nu bigtop;
  # nushell takes several include directories joined by ASCII record separator
  separator = builtins.fromJSON ''"\u001e"'';

  check =
    name:
    let
      dir = pathOf name;
    in
    derivation (
      {
        name = "check-${name}";
        inherit system;
        __contentAddressed = true;
        outputHashMode = "recursive";
        outputHashAlgo = "sha256";
        builder = "${nu}/bin/nu";
        args = [
          "--no-config-file"
          "--include-path=${../../builder}${separator}${./lib}"
          "${dir}/test.nu"
        ];
        PATH = "${nu}/bin:${bigtop}/bin:${set.bootstrap.peanut}/bin:${set.pkgs.rust-bootstrap}/bin";
        fixtures = dir;
        SYS_LIBS = ../../nix/sys-libs.toml;
      }
      // (
        if builtins.elem name built then
          {
            pkg = set.pkgs."test-${name}";
            inherit (set.pkgs."test-${name}") debug;
          }
          // (if builtins.pathExists (dir + "/src") then { src = "${dir}/src"; } else { })
        else
          { }
      )
    );
in
builtins.listToAttrs (
  map (name: {
    inherit name;
    value = check name;
  }) names
)
