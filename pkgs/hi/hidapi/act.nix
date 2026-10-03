{ platform, when, ... }:
[
  (when (platform.os == "linux") {
    cmake.defs.HIDAPI_WITH_HIDRAW = false;
    deps = [ "libusb" ];
  })
]
