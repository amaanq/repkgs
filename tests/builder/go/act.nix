{ fetch, ... }:
[
  {
    go.deps = null;
    source = fetch.files "src" (import ./src.nix);
  }
]
