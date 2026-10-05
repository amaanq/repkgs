{
  tools,
  tree,
  ...
}:
[
  {
    cargo.tool = tools.rust-bootstrap;
    source = tree {
      dir = "tools";
      exclude = [
        "motley"
        "target"
      ];
      files = [
        ".rs"
        "Cargo.toml"
        "Cargo.lock"
        "README.md"
      ];
      name = "mummerd-src";
    };
  }
]
