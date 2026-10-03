{ platform, when, ... }:
[
  (when platform.cross { make.flags = [ "BUILD_CC=cc-build" ]; })
]
