{
  platform,
  source,
  when,
  ...
}:
[
  { env.components = "${toString (source "cargo-riscv64")} ${toString (source "rust-std-riscv64")}"; }
  (when (!platform.cross) {
    env.components = "${toString (source "cargo-x86_64")} ${toString (source "rust-std-x86_64")}";
    source = "rustc-x86_64";
  })
  (when (platform.cpu == "aarch64") {
    env.components = "${toString (source "cargo-aarch64")} ${toString (source "rust-std-aarch64")}";
    source = "rustc-aarch64";
  })
]
