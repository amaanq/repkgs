{ package }:
package {
  name = "alsa-lib";
  platforms.os = [ "linux" ];
  patches = [ ./relocatable.patch ];
  uses = [ "autotools" ];
  autotools.flags = [
    "--with-plugindir=/usr/lib/alsa-lib"
    "--disable-python"
    "--disable-topology"
  ];
}
