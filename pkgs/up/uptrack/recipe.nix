{
  tools,
  tree,
  ...
}:
[
  {
    cargo.tool = tools.rust-bootstrap;
    source = tree {
      dir = "../../ji/jig/src";
      exclude = [ "target" ];
      files = [
        ".rs"
        "Cargo.toml"
        "Cargo.lock"
        "README.md"
      ];
      name = "jig-src";
    };
  }
]
