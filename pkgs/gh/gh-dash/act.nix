{ pin, ... }:
[
  {
    go.ldflags = [
      "-X github.com/dlvhdr/gh-dash/v4/cmd.Version=${pin.version}"
    ];
  }
]
