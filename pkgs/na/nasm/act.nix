{ platform, when, ... }:
[
  # Makefile.in names win/manifest.rc relative to the build directory, which has no win/
  (when (platform.os == "windows") { autotools.outOfTree = false; })
]
