# A package's sources (docs/act.md), with URL placeholders filled the way spieler fills them.
# `unpacker` is a store path holding bin/bsdtar and `mummer` the mummer that fetches and unpacks
# with it. Only the peanut's own sources, a .nar, are read without them.
{
  unpacker,
  mummer ? null,
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
    groupBy
    head
    mapAttrs
    match
    replaceStrings
    stringLength
    substring
    ;
  fetchurl = import <nix/fetchurl.nix>;

  # every spelling of a url, itself first and then its path on each mirror (nix/mirrors.nix).
  # Grouping the prefixes by host keeps the common url to one lookup
  mirrors = import ./mirrors.nix;
  hostOf =
    url:
    let
      found = match "https://([^/]*)/.*" url;
    in
    if found == null then null else head found;
  prefixesOn = groupBy hostOf (attrNames mirrors);
  spellings =
    url:
    let
      host = hostOf url;
      candidates = if host == null then [ ] else prefixesOn.${host} or [ ];
      matching = filter (prefix: substring 0 (stringLength prefix) url == prefix) candidates;
      onMirrors =
        prefix:
        let
          path = substring (stringLength prefix) (-1) url;
        in
        map (mirror: mirror + path) mirrors.${prefix};
    in
    [ url ] ++ concatMap onMirrors matching;

  # the url's file name without its extension (and a .tar before it), or repo-tag for a github tag
  # archive. A fixed-output path is found by name and hash, so a name that follows the url keeps a
  # bumped url with a stale hash from reusing the old download
  storeName =
    url:
    let
      bare = match "((.*)\\.tar|(.*))\\.[a-z0-9]+" url;
      stem =
        if bare == null then
          url
        else if elemAt bare 1 != null then
          elemAt bare 1
        else
          elemAt bare 2;
      github = match "https://github.com/[^/]+/([^/]+)/archive/refs/tags/(.*)" stem;
    in
    replaceStrings [ "~" ] [ "-" ] (
      if github == null then baseNameOf stem else "${elemAt github 0}-${elemAt github 1}"
    );

  # one fixed-output derivation runs `mummer unpack` over the download, so the store holds the
  # unpacked tree and `hash` is that tree's NAR hash, the one spieler computes too
  unpacked =
    source: url:
    derivation {
      inherit system unpacker;
      name = source.name or (storeName url);
      builder = "${mummer}/bin/mummer";
      args = [ "unpack" ];
      urls = spellings url;
      outputHash = source.hash;
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

  # `pin` overrides act.lock values and `hashes` source hashes by name, for overrides (nix/package)
  read =
    pin: hashes: dir:
    let
      act = import ./act.nix dir;
      pinned = act.pinned // pin;
      version = pinned.version or "";
      tag = act.tagOf pinned;

      # {version} and {tag} cover nearly every url, the other placeholders are every act.lock value and
      # three spellings of the version
      everyPlaceholder =
        let
          numbers = match "([^.]*)\\.?([^.]*).*" version;
        in
        {
          tag = version;
          version_ = replaceStrings [ "." ] [ "_" ] version;
          major = elemAt numbers 0;
          minor = elemAt numbers 1;
        }
        // pinned;
      fill =
        template:
        let
          quick = replaceStrings [ "{version}" "{tag}" ] [ version tag ] template;
        in
        if match ".*[{].*" quick == null then
          quick
        else
          replaceStrings (map (key: "{${key}}") (attrNames everyPlaceholder)) (map toString (
            attrValues everyPlaceholder
          )) quick;

      declared = act.sources;
      sources =
        if hashes == { } then
          declared
        else
          mapAttrs (
            key: source: if hashes ? ${key} then source // { hash = hashes.${key}; } else source
          ) declared;

      # `unpack = false` and .nar urls, the peanut among them, go through builtin:fetchurl
      fetch =
        key:
        let
          source = sources.${key} or (throw "${toString dir}: no source '${key}'");
          url = fill source.url;
          nar = match ".*\\.nar(\\.[a-z0-9]+)?" url != null;
        in
        if nar || !(source.unpack or true) then
          fetchurl {
            inherit url;
            name = source.name or (baseNameOf url);
            inherit (source) hash;
            unpack = nar;
          }
        else
          unpacked source url;
    in
    {
      inherit version tag fetch;
      # libraries of the set that the lock files link, which spieler records (builder/sys-libs.json)
      sys = pinned.sys or [ ];
      has = key: sources ? ${key};
      default = fetch "default";
      repin = pin: hashes: read pin hashes dir;
    };
in
read { } { }
