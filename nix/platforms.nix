# Platform facts are bigtop's `targets`, committed in schema.json because the bootstrap reads them
# before bigtop exists.
let
  inherit (builtins.fromJSON (builtins.readFile ./schema.json)) targets;
  # musl only serves the bootstrap and reuses the glibc names
  named = builtins.filter (t: t.libc != "musl") targets;
in
{
  forSystem =
    system: libc:
    let
      cpu = builtins.head (builtins.split "-" system);
    in
    builtins.head (builtins.filter (t: t.os == "linux" && t.cpu == cpu && t.libc == libc) targets);

  byName = builtins.listToAttrs (
    map (t: {
      inherit (t) name;
      value = t;
    }) named
  );

  # mummer adds march and minos itself, the package layer turns interp into a sysroot path and
  # every derivation brings its own system
  forBuilder =
    facts:
    builtins.removeAttrs facts [
      "march"
      "minos"
      "interp"
      "system"
    ];
}
