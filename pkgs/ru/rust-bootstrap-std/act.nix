{
  platform,
  when,
  ...
}:
[
  (when (platform.cpu == "aarch64") { source = "rust-std-aarch64"; })
  (when (platform.cpu == "loongarch64") { source = "rust-std-loongarch64"; })
  (when (platform.cpu == "powerpc64le") { source = "rust-std-powerpc64le"; })
  (when (platform.cpu == "riscv64") { source = "rust-std-riscv64"; })
]
