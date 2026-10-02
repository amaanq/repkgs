{ file, ... }:
[
  { cargo.cratePatches.pest_consume_macros = [ (file "deterministic-parser.patch") ]; }
]
