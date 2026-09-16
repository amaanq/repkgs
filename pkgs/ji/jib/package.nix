{ variant, pkgs }:
variant pkgs.jig {
  cargo.flags.set = [
    "-p"
    "jib"
  ];
  bin.set = [ "jib" ];
}
