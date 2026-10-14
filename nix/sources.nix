# A package directory's sources (docs/act.md) as store paths, URLs filled the way spieler fills
# them. mummer downloads and unpacks with the bsdtar in `unpacker`, only the peanut's own `.nar`
# goes through builtin fetchurl.
{
  unpacker,
  mummer ? null,
  system ? null,
}:
let
  mirrorGroups = builtins.attrValues (builtins.fromTOML (builtins.readFile ./mirrors.toml));

  startsWith = prefix: url: builtins.substring 0 (builtins.stringLength prefix) url == prefix;
  spellings =
    url:
    let
      inGroup =
        group:
        let
          owners = builtins.filter (prefix: startsWith prefix url) group;
          owner = builtins.head owners;
          path = builtins.substring (builtins.stringLength owner) (-1) url;
        in
        if owners == [ ] then
          [ ]
        else
          map (prefix: prefix + path) (builtins.filter (prefix: prefix != owner) group);
    in
    [ url ] ++ builtins.concatMap inGroup mirrorGroups;

  # a fixed-output path is fixed by name and hash, so naming it after the URL keeps a bumped URL
  # with a stale hash from reusing the old download
  storeName =
    url:
    let
      tarball = builtins.match "(.*)\\.tar\\.[a-z0-9]+" url;
      single = builtins.match "(.*)\\.[a-z0-9]+" url;
      stem =
        if tarball != null then
          builtins.head tarball
        else if single != null then
          builtins.head single
        else
          url;
      github = builtins.match "https://github.com/[^/]+/([^/]+)/archive/refs/tags/(.*)" stem;
    in
    builtins.replaceStrings [ "~" ] [ "-" ] (
      if github != null then builtins.concatStringsSep "-" github else baseNameOf stem
    );

  fill =
    pinned: version: tag: template:
    let
      once = builtins.replaceStrings [ "{version}" "{tag}" ] [ version tag ] template;
      parts = builtins.match "([^.]*)\\.?([^.]*).*" version;
      table = {
        tag = version;
        version_ = builtins.replaceStrings [ "." ] [ "_" ] version;
        major = builtins.elemAt parts 0;
        minor = builtins.elemAt parts 1;
      }
      // pinned;
      keys = builtins.attrNames table;
    in
    if builtins.match "[^{]*" once != null then
      once
    else
      builtins.replaceStrings (map (key: "{${key}}") keys) (map (key: toString table.${key}) keys) once;

  record =
    dir: pin: hashes:
    let
      act = import ./act.nix dir;
      pinned = act.pinned // pin;
      version = pinned.version or "";
      tag = act.tagOf pinned;
      sources =
        if hashes == { } then
          act.sources
        else
          builtins.mapAttrs (
            key: source: if hashes ? ${key} then source // { hash = hashes.${key}; } else source
          ) act.sources;

      fetch =
        key:
        let
          source = sources.${key} or (throw "${toString dir}: no source '${key}'");
          url = fill pinned version tag source.url;
          nar = builtins.match ".*\\.nar(\\.[a-z0-9]+)?" url != null;
        in
        if nar || !(source.unpack or true) then
          import <nix/fetchurl.nix> {
            inherit url;
            name = source.name or (baseNameOf url);
            inherit (source) hash;
            unpack = nar;
          }
        else
          # mummer unpacks inside the fixed-output build, so `hash` is the unpacked tree's, as
          # spieler computes it
          derivation {
            name = source.name or (storeName url);
            inherit system unpacker;
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
    in
    {
      inherit version tag fetch;
      expand = fill pinned version tag;
      sys = pinned.sys or [ ];
      has = key: sources ? ${key};
      default = fetch "default";
      # spieler keys a Cargo.lock's git sources by the lock's own source string
      git = builtins.listToAttrs (
        map (key: {
          name = key;
          value = fetch key;
        }) (builtins.filter (startsWith "git+") (builtins.attrNames sources))
      );
      repin = record dir;
    };
in
dir: record dir { } { }
