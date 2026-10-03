{
  pin,
  source,
  fetch,
  ...
}:
[
  {
    go.ldflags = [
      "-X github.com/navidrome/navidrome/consts.gitTag=v${pin.version}"
      "-X github.com/navidrome/navidrome/consts.gitSha=nix"
    ];
    npm.deps = fetch.npmDeps {
      root = "ui";
      source = source "default";
    };
  }
]
