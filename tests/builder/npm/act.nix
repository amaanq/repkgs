{ fetch, ... }:
[
  {
    npm.script = null;
    source = fetch.files "src" (import ./src.nix);
  }
]
