# Every platform's facts, as bigtop's targets module emits them into nix/schema.json. Bootstrap reads
# this before bigtop exists, which is why the facts are committed rather than computed at eval time.
let
  inherit (builtins.fromJSON (builtins.readFile ./schema.json)) targets;
  linux = builtins.filter (target: target.os == "linux") targets;
in
{
  # the Linux platform of a nix system (`x86_64-linux`) with that libc, glibc or musl
  forSystem =
    system: libc:
    let
      cpu = builtins.head (builtins.split "-" system);
    in
    builtins.head (builtins.filter (target: target.cpu == cpu && target.libc == libc) linux);
  # every target platform by its name, musl only serves the bootstrap
  byName = builtins.listToAttrs (
    map (target: {
      inherit (target) name;
      value = target;
    }) (builtins.filter (target: target.libc != "musl") targets)
  );
  # the facts a derivation hands bigtop: mummer adds march and minos to every compile itself, the
  # package layer turns interp into a sysroot path, and system is the derivation's own
  forBuilder =
    facts:
    removeAttrs facts [
      "march"
      "minos"
      "interp"
      "system"
    ];
}
