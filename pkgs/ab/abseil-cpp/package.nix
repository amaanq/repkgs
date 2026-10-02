{
  package,
  pkgs,
  sources,
}:
package {
  name = "abseil-cpp";
  uses = [ "cmake" ];
  cmake.defs = {
    CMAKE_CXX_STANDARD = "17";
    ABSL_PROPAGATE_CXX_STD = true;
    ABSL_BUILD_TESTING = true;
    ABSL_USE_EXTERNAL_GOOGLETEST = true;
  };
  dependencies = [ pkgs.googletest ];
  # cctz's tests load zones from $TZDIR, and the tarball carries the ones they expect
  env.TZDIR = "${sources.fetch "default"}/absl/time/internal/cctz/testdata/zoneinfo";
}
