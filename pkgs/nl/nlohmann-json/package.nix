{
  package,
  buildPkgs,
}:
package {
  name = "nlohmann-json";
  uses = [ "cmake" ];
  cmake.tool = buildPkgs.cmake-bootstrap; # cmake links cppdap, which wants this
  cmake.defs = {
    JSON_BuildTests = false;
    JSON_MultipleHeaders = true;
  };
}
