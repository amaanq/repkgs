{
  tools,
  tree,
  ...
}:
[
  {
    cargo.tool = tools.rust-bootstrap;
    source = tree {
      dir = "../../mu/mummer/src";
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
