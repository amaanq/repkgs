{ package }:
package {
  name = "nushell";
  uses = [ "cargo" ];
  # default features minus plugin + system-clipboard (x11/wayland)
  cargo.noDefaultFeatures = true;
  cargo.features = [
    "network"
    "sqlite"
    "trash-support"
    "rustls-tls"
  ];
  cargo.cratePatches.pest_consume_macros = [ ./deterministic-parser.patch ];
  tests.run = false; # the suite spawns `nu` against a home directory and a tty
  bin = [ "nu" ];
}
