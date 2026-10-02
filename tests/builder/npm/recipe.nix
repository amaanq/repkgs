{ tree, ... }:
[
  {
    npm.script = null;
    source = tree {
      dir = "src";
      name = "src";
    };
  }
]
