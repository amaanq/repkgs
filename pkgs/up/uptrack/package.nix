{ variant, pkgs }:
variant pkgs.jig {
  cargo.flags.set = [
    "-p"
    "uptrack"
  ];
  bin.set = [ "uptrack" ];
}
