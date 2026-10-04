{ source, fetch, ... }:
[
  {
    npm.deps = fetch.npmDeps {
      root = "ui";
      source = source "default";
    };
  }
]
