{
  platform,
  when,
  ...
}:
[
  (when (platform.cpu == "riscv64") {
    deps = [
      "numactl"
      "llvm"
    ];
    version = "9.10.3";
  })
]
