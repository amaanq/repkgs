{
  platform,
  source,
  file,
  ...
}:
let
  fixtures = builtins.attrNames (builtins.fromTOML (builtins.readFile (file "act.lock"))).sources;
in
[
  {
    make.flags = [
      "CC=cc"
      "STRIP="
      "MACHINE=${platform.gnuTriple}"
    ];
    env.TREE_SITTER_FIXTURES = toString (map source fixtures);
  }
]
