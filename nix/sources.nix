# The sources half of a recipe.toml (docs/uptrack.md), with URL placeholders filled the way
# uptrack fills them. `unpacker` is a store path holding bin/bsdtar and `jig` the jig that fetches
# and unpacks with it. Only the seed's own recipe.toml, a .nar, is read without them.
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

  # one fixed-output derivation runs `jig unpack` over the download, so the store holds the
  # unpacked tree and `hash` is that tree's NAR hash, the one uptrack computes too
  unpacked =
    source: url:
    derivation {
      inherit system unpacker;
      name = source.name or (storeName url);
      builder = "${jig}/bin/jig";
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

  # `pin` overrides [pin] keys and `hashes` source hashes by key, for overrides (nix/package)
  read =
    pin: hashes: file:
    let
      table = fromTOML (readFile file);
      pinned = (table.pin or { }) // pin;
      version = pinned.version or "";
      tag = pinned.tag or version;

      # {version} and {tag} cover nearly every url, the other placeholders are every [pin] key and
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

      listed = table.source or [ ];
      declared =
        if length listed == 1 && (head listed).key == "default" then
          { default = head listed; }
        else
          listToAttrs (
            map (source: {
              name = source.key;
              value = source;
            }) listed
          );
      sources =
        if hashes == { } then
          declared
        else
          mapAttrs (
            key: source: if hashes ? ${key} then source // { hash = hashes.${key}; } else source
          ) declared;

      # `unpack = false` and .nar urls, the seed among them, go through builtin:fetchurl
      fetch =
        key:
        let
          source = sources.${key} or (throw "${toString file}: no source '${key}'");
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
      # libraries of the set that the lock files link, which uptrack records (builder/sys-libs.json)
      sys = pinned.sys or [ ];
      has = key: sources ? ${key};
      default = fetch "default";
      repin = pin: hashes: read pin hashes file;
    };
in
read { } { }
