{
  fetch,
  source,
  tools,
  ...
}:
[
  {
    # the tests build crates from crates.io, and custom_target builds std with -Zbuild-std
    cargo.deps = fetch.cargoVendor {
      source = source "default";
      locks = [
        "test/config_discovery/config_discovery/Cargo.lock"
        "test/custom_target/custom_target/rust/Cargo.lock"
        "test/hostbuild/hostbuild/Cargo.lock"
        "${tools.rust}/lib/rustlib/src/rust/library/Cargo.lock"
      ];
    };
  }
]
