{ platform, when, ... }:
[
  (when (platform.os == "macos") { autotools.flags = [ "--disable-symvers" ]; })
  (when (platform.os == "windows") { autotools.flags = [ "--disable-symvers" ]; })
]
