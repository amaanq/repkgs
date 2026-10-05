{ fetch, ... }:
[
  {
    cargo.deps = null;
    source = fetch.files "src" (import ./src.nix);
  }
]
