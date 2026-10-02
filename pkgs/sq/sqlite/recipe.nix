{ platform, when, ... }:
[
  (when platform.cross { make.configureFlags = [ "--host=${platform.gnuTriple}" ]; })
  (when (platform.os == "linux") {
    make.flags = [ "LDFLAGS.libsqlite3.soname=-Wl,-soname,libsqlite3.so.0" ];
  })
]
