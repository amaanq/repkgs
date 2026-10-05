{ tree, ... }:
[
  {
    npm.script = null;
    source = tree {
      dir = "tests/builder/npm/src";
      name = "src";
    };
  }
]
