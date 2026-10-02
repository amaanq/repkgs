{ platform, when, ... }:
[
  (when (platform.os == "windows") { needs = [ "libiconv" ]; })
]
