{ tree, ... }:
[
  {
    cargo.deps = null;
    source = tree {
      dir = "src";
      name = "src";
    };
  }
]
