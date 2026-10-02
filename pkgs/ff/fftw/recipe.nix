{ platform, when, ... }:
[
  (when (platform.os == "windows") {
    autotools = {
      flags = [ "--with-combined-threads" ];
      makeFlags = [ "LDFLAGS=-no-undefined" ];
    };
  })
]
