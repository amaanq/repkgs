{ package }:
package {
  name = "attr";
  platforms.os = [ "linux" ];
  patches = [ ./relocatable.patch ];
  uses = [ "autotools" ];
  bin = [
    "getfattr"
    "setfattr"
    "attr"
  ];
  tests.run = false;
}
