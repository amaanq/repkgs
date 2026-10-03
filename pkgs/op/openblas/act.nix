{
  platform,
  pkgs,
  when,
  ...
}:
[
  (when (!platform.cross) {
    make.flags = [
      "TARGET=HASWELL"
      "HOSTCC=$(CC_FOR_BUILD)"
      "CROSS=0"
      "FC=${pkgs.flang-rt}/bin/flang"
      "NUM_THREADS=64"
      "USE_OPENMP=0"
    ];
  })
  (when (platform.cpu == "aarch64") {
    make.flags = [
      "TARGET=ARMV8"
      "HOSTCC=$(CC_FOR_BUILD)"
      "CROSS=1"
      "FC=${pkgs.flang-rt}/bin/flang"
      "NUM_THREADS=64"
      "USE_OPENMP=0"
    ];
  })
  (when (platform.cpu == "loongarch64") {
    make.flags = [
      "TARGET=LOONGSONGENERIC"
      "HOSTCC=$(CC_FOR_BUILD)"
      "CROSS=1"
      "FC=${pkgs.flang-rt}/bin/flang"
      "NUM_THREADS=64"
      "USE_OPENMP=0"
    ];
  })
  (when (platform.cpu == "powerpc64le") {
    make.flags = [
      "TARGET=POWER9"
      "HOSTCC=$(CC_FOR_BUILD)"
      "CROSS=1"
      "FC=${pkgs.flang-rt}/bin/flang"
      "NUM_THREADS=64"
      "USE_OPENMP=0"
    ];
  })
  (when (platform.cpu == "riscv64") {
    make.flags = [
      "TARGET=RISCV64_GENERIC"
      "HOSTCC=$(CC_FOR_BUILD)"
      "CROSS=1"
      "FC=${pkgs.flang-rt}/bin/flang"
      "NUM_THREADS=64"
      "USE_OPENMP=0"
    ];
  })
]
