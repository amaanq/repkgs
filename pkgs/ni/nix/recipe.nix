{ platform, when, ... }:
[
  (when (!platform.cross) {
    meson.defs."libutil:cpuid" = "enabled";
    needs = [ "libcpuid" ];
  })
  (when (platform.os == "linux") {
    meson.defs."libstore:seccomp-sandboxing" = "enabled";
    needs = [ "libseccomp" ];
  })
]
