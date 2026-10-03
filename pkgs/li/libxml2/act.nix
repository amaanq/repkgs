{ platform, when, ... }:
[
  (when (platform.os == "windows") { deps = [ "libiconv" ]; })
]
