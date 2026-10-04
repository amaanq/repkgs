# A workspace crate built by nixpkgs' rust for the development shell, nuff and the mummerd service,
# so none of them start the package set.
{ pkgs, crate }:
pkgs.rustPlatform.buildRustPackage {
  pname = crate;
  version =
    (builtins.fromTOML (builtins.readFile ../pkgs/mu/mummer/src/Cargo.toml)).workspace.package.version;
  src = builtins.path {
    path = ../pkgs/mu/mummer/src;
    name = "${crate}-rust-src";
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
    crate
  ];
  auditable = false;
  doCheck = false;
  env.MUMMER_STORE_DIR = builtins.storeDir;
}
