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
