{
  platform,
  when,
  ...
}:
[
  (when (platform.cpu == "riscv64") {
    needs = [
      "numactl"
      "llvm"
    ];
    version = "9.10.3";
  })
  (when (platform.cpu != "riscv64") { tools = [ "cpython" ]; })
]
