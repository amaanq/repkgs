# spieler built by nixpkgs' rust for the development shell and nuff, so a tree check does not
# start the package set. The set's own build is pkgs/sp/spieler.
{ pkgs }:
pkgs.rustPlatform.buildRustPackage {
  pname = "spieler";
  version =
    (builtins.fromTOML (builtins.readFile ../pkgs/mu/mummer/src/Cargo.toml)).workspace.package.version;
  src = builtins.path {
    path = ../pkgs/mu/mummer/src;
    name = "spieler-rust-src";
    filter =
      path: type:
      (type == "directory" && baseNameOf path != "target")
      || builtins.match ".*\\.rs" path != null
      || builtins.elem (baseNameOf path) [
        "Cargo.toml"
        "Cargo.lock"
      ];
  };
  cargoLock.lockFile = ../pkgs/mu/mummer/src/Cargo.lock;
  cargoBuildFlags = [
    "-p"
    "spieler"
  ];
  auditable = false;
  doCheck = false;
  env.MUMMER_STORE_DIR = builtins.storeDir;
}
