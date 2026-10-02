{ platform, when, ... }:
[
  (when (!platform.cross) { install."bin/ninja" = "../build/ninja"; })
  (when (platform.os == "linux") { install."bin/ninja" = "../build/ninja"; })
  (when (platform.os == "macos") { install."bin/ninja" = "../build/ninja"; })
  (when (platform.os == "windows") { install."bin/ninja.exe" = "../build/ninja.exe"; })
]
