{
  tools,
  tree,
  ...
}:
[
  {
    cargo.tool = tools.rust-bootstrap;
    source = tree {
      dir = "../shim/src";
      exclude = [ "target" ];
      name = "shimd-src";
    };
  }
]
