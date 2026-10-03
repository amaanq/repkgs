{ platform, when, ... }:
[
  (when (!platform.cross) {
    only.os = [
      "linux"
      "macos"
    ];
  })
  (when (platform.os == "windows") {
    only.os = [
      "linux"
      "macos"
    ];
  })
  (when (platform.libc == "msvc") { cc.cflags = [ "-Dpid_t=int" ]; })
]
