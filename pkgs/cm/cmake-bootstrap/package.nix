# cmake built by its ./bootstrap (make, bundled libraries), for the build machine only: the cmake
# that builds zlib, zstd, expat, curl, libarchive and then cmake proper, which link those
{ package }:
package {
  name = "cmake-bootstrap";
  uses = [ "make" ];
  phases = [
    "boot.configure"
    "make.build"
    "make.install"
  ];
  platforms.cross = false;
  tests.run = false;
  bin = [
    "cmake"
    "ctest"
    "cpack"
  ];
}
