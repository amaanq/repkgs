{ tree, ... }:
[
  {
    go.deps = null;
    source = tree {
      dir = "tests/builder/go/src";
      name = "src";
    };
  }
]
