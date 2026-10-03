# What a package.nix wrote, after overrides, over what its act.toml supplies: version and source
# unless the package sets them (local trees, demos)
{
  platform,
  lib,
  # overrides applied to a spec before validation: name -> spec -> spec (nix/overrides.nix)
  edit,
}:
{ sources, written }:
let
  completions = import ./completions.nix;
  edited = if edit == null then written else edit written.name written;
  # an override may repin the package: `pin.merge = { version = "…"; }` plus
  # `hash.merge = { default = "sha256-…"; }` re-read the sources under the new pin
  repinned = edit != null && sources != null && (edited ? pin || edited ? hash);
  pinned = if repinned then sources.repin (edited.pin or { }) (edited.hash or { }) else sources;
  fromSources = lib.on (pinned != null) {
    inherit (pinned) version;
    # one tarball for all, or one per platform ("x86_64-linux", "aarch64-macos")
    source = if pinned.has "default" then "default" else "${platform.cpu}-${platform.os}";
  };
  own =
    if repinned then
      removeAttrs edited [
        "pin"
        "hash"
      ]
    else
      edited;
in
{
  sources = pinned;
  args =
    fromSources
    // own
    // lib.on (edited ? completions) {
      install = completions edited.completions // (edited.install or { });
    };
}
