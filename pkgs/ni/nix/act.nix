{
  platform,
  pkgs,
  when,
  ...
}:
[
  # the public headers include boost and nlohmann-json, which the .pc files do not require
  {
    exports.propagate = [
      "${pkgs.boost}"
      "${pkgs.nlohmann-json}"
    ];
  }
  (when (!platform.cross) {
    meson.defs."libutil:cpuid" = "enabled";
    deps = [ "libcpuid" ];
  })
  (when (platform.os == "linux") {
    meson.defs."libstore:seccomp-sandboxing" = "enabled";
    meson.defs."libstore:sandbox-shell" = "${pkgs.dash-static}/bin/dash";
    deps = [
      "dash-static"
      "libseccomp"
    ];
  })
]
