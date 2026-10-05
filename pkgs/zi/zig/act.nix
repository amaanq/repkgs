{
  platform,
  pkgs,
  tools,
  when,
  ...
}:
[
  (when platform.cross {
    cmake.defs = {
      CMAKE_PROGRAM_PATH = "${pkgs.llvm22}/host";
      ZIG_EXECUTABLE = "${tools.zig}/bin/zig";
      ZIG_STATIC_ZLIB = true;
      ZIG_STATIC_ZSTD = true;
      ZIG_USE_LLVM_CONFIG = true;
    };
  })
  (when (platform.os == "macos") { cmake.defs.ZIG_TARGET_TRIPLE = "aarch64-linux-gnu.2.44"; })
  (when (platform.os == "windows") { cmake.defs.ZIG_TARGET_TRIPLE = "x86_64-linux-gnu.2.44"; })
  (when (platform.cpu == "aarch64") { cmake.defs.ZIG_TARGET_TRIPLE = "aarch64-linux-gnu.2.44"; })
  (when (platform.cpu == "loongarch64") {
    cmake.defs.ZIG_TARGET_TRIPLE = "loongarch64-linux-gnu.2.44";
  })
  (when (platform.cpu == "powerpc64le") {
    cmake.defs.ZIG_TARGET_TRIPLE = "powerpc64le-linux-gnu.2.44";
  })
  (when (platform.cpu == "riscv64") { cmake.defs.ZIG_TARGET_TRIPLE = "riscv64-linux-gnu.2.44"; })
]
