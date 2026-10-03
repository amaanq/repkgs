{
  tools,
  tree,
  ...
}:
[
  {
    cargo.tool = tools.rust-bootstrap;
    source = tree {
      dir = "../mummer/src";
      exclude = [ "target" ];
      name = "mummerd-src";
    };
  }
]
