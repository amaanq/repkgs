# plumb built by nixpkgs' rust for the development shell and nuff, so a tree check does not
# start the package set. The set's own build is pkgs/pl/plumb.
{ pkgs }:
pkgs.rustPlatform.buildRustPackage {
  pname = "plumb";
  version =
    (builtins.fromTOML (builtins.readFile ../pkgs/sh/shim/src/Cargo.toml)).workspace.package.version;
  src = builtins.path {
    path = ../pkgs/sh/shim/src;
    name = "plumb-rust-src";
    filter =
      path: type:
      (type == "directory" && baseNameOf path != "target")
      || builtins.match ".*\\.rs" path != null
      || builtins.elem (baseNameOf path) [
        "Cargo.toml"
        "Cargo.lock"
      ];
  };
  cargoLock.lockFile = ../pkgs/sh/shim/src/Cargo.lock;
  cargoBuildFlags = [
    "-p"
    "plumb"
  ];
  auditable = false;
  doCheck = false;
  env.SHIM_STORE_DIR = builtins.storeDir;
}
