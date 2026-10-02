{
  tools,
  tree,
  ...
}:
[
  {
    cargo.tool = tools.rust-bootstrap;
    source = tree {
      dir = "../jig/src";
      exclude = [ "target" ];
      name = "jigd-src";
    };
  }
]
