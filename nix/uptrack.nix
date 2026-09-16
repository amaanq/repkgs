# uptrack built by nixpkgs' rust for the development shell and treefmt, so a tree check does not
# start the package set. The set's own build is pkgs/up/uptrack.
{ pkgs }:
pkgs.rustPlatform.buildRustPackage {
  pname = "uptrack";
  version = (builtins.fromTOML (builtins.readFile ../pkgs/ji/jig/src/Cargo.toml)).workspace.package.version;
  src = builtins.path {
    path = ../pkgs/ji/jig/src;
    name = "uptrack-rust-src";
    filter =
      path: type:
      (type == "directory" && baseNameOf path != "target")
      || builtins.match ".*\\.rs" path != null
      || builtins.elem (baseNameOf path) [
        "Cargo.toml"
        "Cargo.lock"
      ];
  };
  cargoLock.lockFile = ../pkgs/ji/jig/src/Cargo.lock;
  cargoBuildFlags = [
    "-p"
    "uptrack"
  ];
  auditable = false;
  doCheck = false;
  env.JIG_STORE_DIR = builtins.storeDir;
}
