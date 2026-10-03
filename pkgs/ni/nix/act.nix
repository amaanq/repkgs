{ platform, when, ... }:
[
  (when (!platform.cross) {
    meson.defs."libutil:cpuid" = "enabled";
    deps = [ "libcpuid" ];
  })
  (when (platform.os == "linux") {
    meson.defs."libstore:seccomp-sandboxing" = "enabled";
    deps = [ "libseccomp" ];
  })
]
