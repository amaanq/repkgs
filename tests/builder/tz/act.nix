{ fetch, ... }:
[
  {
    source = fetch.files "src" (import ./src.nix);
  }
]
