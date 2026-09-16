# GHC -jsem tokens served from jigd slots, jib's cabal system runs ghc under it
{ variant, pkgs }:
variant pkgs.jig {
  cargo.flags.set = [
    "-p"
    "jsem"
  ];
  bin.set = [ "jsem" ];
}
