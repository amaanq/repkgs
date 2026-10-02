{ package, pkgs }:
package {
  name = "acl";
  platforms.os = [ "linux" ];
  uses = [ "autotools" ];
  dependencies = [ pkgs.attr ];
  tests.run = false;
}
