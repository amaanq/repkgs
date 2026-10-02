{ platform, when, ... }:
[
  {
    make.configureFlags = [
      platform.opensslTarget
      "--libdir=lib"
      "--openssldir=/etc/ssl"
      "shared"
      "no-tests"
    ];
  }
  (when (platform.os == "linux") { make.configureFlags = [ "enable-ktls" ]; })
]
