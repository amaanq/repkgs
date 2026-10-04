{ pin, ... }:
[
  {
    go.ldflags = [
      "-X main.version=v${pin.version}"
    ];
  }
]
