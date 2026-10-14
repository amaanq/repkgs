{ platform, when, ... }:
[
  (when (platform.cpu == "x86_64") {
    meson.defs.intel = "enabled";
    meson.defs.vmwgfx = "enabled";
    deps = [ "libpciaccess" ];
  })
  (when (platform.cpu == "aarch64") {
    meson.defs = {
      freedreno = "enabled";
      etnaviv = "enabled";
      vc4 = "enabled";
      omap = "enabled";
      tegra = "enabled";
      exynos = "enabled";
    };
  })
]
