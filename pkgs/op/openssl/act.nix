{ platform, when, ... }:
[
  {
    make.configureFlags = [
      platform.opensslTarget
      "--libdir=lib"
      "--openssldir=/etc/ssl"
      "shared"
    ];
  }
  (when (platform.os == "linux") {
    make.configureFlags = [
      "enable-ktls"
      "no-afalgeng"
    ];
  })
  (when (platform.abi == "msvc") { tools = [ "nasm" ]; })
]
