# Helpers shared by package.nix files and nix/*.nix, passed in as `lib` (and `on` alone, since it
# is everywhere). A helper names what it returns, takes its data last, and joins this file once a
# third caller wants it. Code that runs once per package in nix/package writes the `if` inline
# instead, since every call allocates thunks the inline form does not.
let
  inherit (builtins) concatStringsSep isAttrs;
in
rec {
  # `++ on platform.cross [ buildPkgs.x ]`, `// on cond { … }`: the value when the condition
  # holds, else the empty list or set. Contents stay lazy; not for strings, which have none
  on =
    condition: value:
    if condition then
      value
    else if isAttrs value then
      { }
    else
      [ ];
  # `per platform.os { linux = "linux"; macos = "macosx"; default = null; }`: the entry for
  # `key`, else `default`, else an error naming the key
  per = key: table: table.${key} or table.default or (throw "no entry for ${key}");
  # space separated is `toString list`
  join = concatStringsSep;
  lines = join "\n";
}
