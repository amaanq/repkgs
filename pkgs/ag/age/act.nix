{ pin, ... }:
[
  {
    go.ldflags = [
      "-X main.Version=${pin.version}"
    ];
  }
]
