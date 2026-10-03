{ platform, when, ... }:
[
  (when (platform.os == "linux") { make.flags = [ "SOEXT=.so" ]; })
  (when (platform.os == "macos") { make.flags = [ "SOEXT=.dylib" ]; })
  (when (platform.os == "windows") { make.flags = [ "SOEXT=.dll" ]; })
]
