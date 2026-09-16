{ package, buildPkgs }:
package {
  name = "jig";
  platforms.posix = true;
  version = (builtins.fromTOML (builtins.readFile ./src/Cargo.toml)).workspace.package.version;
  source = builtins.path {
    path = ./src;
    name = "jig-src";
    filter =
      path: type:
      (type == "directory" && builtins.baseNameOf path != "target")
      || builtins.match ".*\\.rs" path != null
      || builtins.elem (builtins.baseNameOf path) [
        "Cargo.toml"
        "Cargo.lock"
      ];
  };
  uses = [ "cargo" ];
  cargo.tool = buildPkgs.rust-bootstrap;
  cargo.flags = [
    "-p"
    "jig"
  ];
  env.JIG_STORE_DIR = builtins.storeDir;
  bin = [ "jig" ];
  tests.version = false;
}
