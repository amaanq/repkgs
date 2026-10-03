{
  tools,
  tree,
  ...
}:
[
  {
    cargo.tool = tools.rust-bootstrap;
    source = tree {
      dir = "src";
      exclude = [ "target" ];
      files = [
        ".rs"
        "Cargo.toml"
        "Cargo.lock"
        "README.md"
      ];
      name = "mummer-src";
    };
  }
]
