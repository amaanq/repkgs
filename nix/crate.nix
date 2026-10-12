# A workspace crate built by nixpkgs' rust for the development shell, nuff and the mummerd service,
# so none of them start the package set.
{
  pkgs,
  crate,
  features ? [ ],
  env ? { },
}:
let
  workspace =
    if
      builtins.elem crate [
        "mummer"
        "tightrope"
      ]
    then
      ../tools/bootstrap
    else
      ../tools;
in
pkgs.rustPlatform.buildRustPackage {
  pname = crate;
  version =
    (builtins.fromTOML (builtins.readFile (workspace + "/Cargo.toml"))).workspace.package.version;
  src = builtins.path {
    path = workspace;
    name = "${crate}-rust-src";
    filter =
      path: type:
      (type == "directory" && baseNameOf path != "target")
      || builtins.match ".*\\.(rs|c)" path != null
      || builtins.elem (baseNameOf path) [
        "Cargo.toml"
        "Cargo.lock"
      ];
  };
  cargoLock.lockFile = workspace + "/Cargo.lock";
  cargoBuildFlags = [
    "-p"
    crate
  ];
  buildFeatures = features;
  auditable = false;
  doCheck = false;
  env = {
    MUMMER_STORE_DIR = builtins.storeDir;
  }
  // env;
}
