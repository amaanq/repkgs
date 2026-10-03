{ platform, when, ... }:
[
  # CMakeLists.txt includes GNUInstallDirs only if(UNIX) but installs to its variables everywhere
  (when (platform.os == "windows") {
    cmake.defs = {
      CMAKE_INSTALL_BINDIR = "bin";
      CMAKE_INSTALL_LIBDIR = "lib";
      CMAKE_INSTALL_INCLUDEDIR = "include";
    };
  })
]
