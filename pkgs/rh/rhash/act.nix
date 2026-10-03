{ platform, when, ... }:
[
  { make.configureFlags = [ "--target=${platform.gnuTriple}" ]; }
  (when (platform.os != "windows") { make.installTarget = [ "install-so-link" ]; })
]
