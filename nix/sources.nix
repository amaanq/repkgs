# Reads a sources.toml (docs/uptrack.md). URL templates expand like uptrack `expand`.
# `unpacker`: store path with bin/bsdtar (the seed), `jig`: the jig that downloads and unpacks.
# Only the seed's own sources.toml (a .nar) is read without them.
{
  unpacker,
  jig ? null,
  system ? null,
}:
let
  inherit (builtins)
    attrNames
    attrValues
    baseNameOf
    concatMap
    elemAt
    filter
    fromTOML
    groupBy
    head
    length
    listToAttrs
    mapAttrs
    match
    readFile
    replaceStrings
    stringLength
    substring
    ;
  fetchurl = import <nix/fetchurl.nix>;
  mirrors = import ./mirrors.nix;
  hostOf =
    url:
    let
      found = match "https://([^/]*)/.*" url;
    in
    if found == null then null else head found;
  startsWith = prefix: text: substring 0 (stringLength prefix) text == prefix;
  # the url itself, then the same path on every mirror of its prefix (nix/mirrors.nix). Mirrors
  # are indexed by host so most urls cost one lookup, not a scan
  prefixesByHost = groupBy hostOf (attrNames mirrors);
  alternatives =
    url:
    let
      host = hostOf url;
      prefixes = filter (prefix: startsWith prefix url) (
        if host == null then [ ] else prefixesByHost.${host} or [ ]
      );
      rest = prefix: substring (stringLength prefix) (-1) url;
    in
    [ url ] ++ concatMap (prefix: map (mirror: mirror + rest prefix) mirrors.${prefix}) prefixes;

  # "llvm-project-21.1.8.src" from …/llvm-project-21.1.8.src.tar.xz, "fd-v10.5.0" for github tag archives
  # strip one extension, and a ".tar" before it
  withoutExtension =
    name:
    let
      parts = match "((.*)\\.tar|(.*))\\.[a-z0-9]+" name;
    in
    if parts == null then
      name
    else if elemAt parts 1 != null then
      elemAt parts 1
    else
      elemAt parts 2;
  downloadName =
    url:
    let
      stem = withoutExtension url;
      github = match "https://github.com/[^/]+/([^/]+)/archive/refs/tags/(.*)" stem;
    in
    replaceStrings [ "~" ] [ "-" ] (
      if github != null then "${elemAt github 0}-${elemAt github 1}" else baseNameOf stem
    );

  # what every fetch-and-unpack derivation shares
  unpacking = {
    inherit system unpacker;
    builder = "${jig}/bin/jig";
    args = [ "unpack" ];
    outputHashMode = "recursive";
    preferLocalBuild = true;
    impureEnvVars = [
      "http_proxy"
      "https_proxy"
      "ftp_proxy"
      "all_proxy"
      "no_proxy"
    ];
  };

  # `pin`: [pin] keys over the file's, `hashes`: source key -> hash (nix/package, overrides)
  read =
    pin: hashes: file:
    let
      table = fromTOML (readFile file);
      pinned = (table.pin or { }) // pin;
      version = pinned.version or "";
      tag = pinned.tag or version;
      # every [pin] key is a {key} placeholder, plus three spellings derived from the version.
      # Nearly every url uses {version} and {tag} only: the full table is built for the rest
      placeholders =
        let
          parts = match "([^.]*)\\.?([^.]*).*" version;
        in
        {
          tag = version;
          version_ = replaceStrings [ "." ] [ "_" ] version;
          major = elemAt parts 0;
          minor = elemAt parts 1;
        }
        // pinned;
      expand =
        template:
        let
          common = replaceStrings [ "{version}" "{tag}" ] [ version tag ] template;
        in
        if match ".*[{].*" common == null then
          common
        else
          replaceStrings (map (key: "{${key}}") (attrNames placeholders)) (map toString (
            attrValues placeholders
          )) common;
      listed = table.source or [ ];
      # one `default` source is the common case: no table
      keyed =
        if length listed == 1 && (head listed).key == "default" then
          { default = head listed; }
        else
          listToAttrs (
            map (source: {
              name = source.key;
              value = source;
            }) listed
          );
      byKey =
        if hashes == { } then
          keyed
        else
          mapAttrs (
            key: source: source // (if hashes ? ${key} then { hash = hashes.${key}; } else { })
          ) keyed;
      fetch =
        key:
        let
          source = byKey.${key} or (throw "${toString file}: no source '${key}'");
          url = expand source.url;
          nar = match ".*\\.nar(\\.[a-z0-9]+)?" url != null;
        in
        # `hash` is the NAR hash of what lands in the store: for archives the unpacked tree (single
        # top-level directory stripped, u+w; jig's unpack, shared with uptrack so the hashes agree), fetched and
        # unpacked by one fixed-output derivation running `jig unpack` (rustls with built-in roots)
        # and the seed's bsdtar, so builds copy a directory instead of decompressing every time.
        # `unpack = false` and .nar urls (the seed itself) go through builtin:fetchurl.
        if !(source.unpack or true) || nar then
          fetchurl {
            inherit url;
            name = source.name or (baseNameOf url);
            inherit (source) hash;
            unpack = nar;
          }
        else
          derivation (
            unpacking
            // {
              # a fixed-output path is found by (name, hash): with a constant name a bumped url whose
              # hash was not updated silently reuses the old download, so the name follows the url
              name = source.name or (downloadName url);
              urls = alternatives url;
              outputHash = source.hash;
            }
          );
    in
    {
      inherit version tag fetch;
      # our libraries the lock file links (builder/sys-libs.json), written by uptrack
      sys = pinned.sys or [ ];
      has = key: byKey ? ${key};
      default = fetch "default";
      # the same file under another pin (nix/package, for overrides)
      repin = pin: hashes: read pin hashes file;
    };
in
read { } { }
