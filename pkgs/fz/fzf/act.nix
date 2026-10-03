{ pin, ... }:
[
  {
    go.ldflags = [
      "-X main.version=${pin.version}"
      "-X main.revision=nix1"
    ];
  }
]
