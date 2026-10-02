{ platform, when, ... }:
[
  (when (!platform.cross) {
    targets.os = [
      "linux"
      "macos"
    ];
  })
  (when (platform.os == "windows") {
    targets.os = [
      "linux"
      "macos"
    ];
  })
  (when (platform.libc == "msvc") { cc.cflags = [ "-Dpid_t=int" ]; })
]
