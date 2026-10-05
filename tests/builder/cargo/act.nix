{ tree, ... }:
[
  {
    cargo.deps = null;
    source = tree {
      dir = "tests/builder/cargo/src";
      name = "src";
    };
  }
]
