# A package directory's act.toml and act.lock (docs/act.md) as the loaders use them: the pinned
# values, the tag, and every source by name with its url template and hash.
dir:
let
  inherit (builtins)
    attrNames
    elem
    filter
    fromTOML
    head
    isAttrs
    isString
    mapAttrs
    match
    pathExists
    readFile
    removeAttrs
    replaceStrings
    ;
  act = fromTOML (readFile (dir + "/act.toml"));
  lock = if pathExists (dir + "/act.lock") then fromTOML (readFile (dir + "/act.lock")) else { };
  where = "${toString dir}/act.toml";

  track = if isAttrs (act.track or null) then act.track.from or null else act.track or null;
  repo = match "github:([^/?]+/[^/?]+)" (if track == null then "" else track);
  forge =
    if repo == null then
      throw "${where}: asset and archive need a github track"
    else
      "https://github.com/${head repo}";
  url =
    source:
    source.url or (
      if source ? asset then
        "${forge}/releases/download/{tag}/${source.asset}"
      else if source.archive or false then
        "${forge}/archive/refs/tags/{tag}.tar.gz"
      else
        throw "${where}: a source needs url, asset or archive"
    );

  single = act ? fetch && (act.fetch ? url || act.fetch ? asset || act.fetch ? archive);
  from = act.fetch.from or null;
  declared =
    if single then
      { default = act.fetch; }
    else if act ? fetch && from == null then
      act.fetch
    else
      { };
  hashes = if isString (lock.hash or null) then { default = lock.hash; } else lock.hash or { };

  identity = [
    "name"
    "tag"
    "track"
    "fetch"
  ];
in
{
  inherit act from;
  # every url placeholder spieler resolved
  pinned =
    (lock.vars or { })
    // removeAttrs lock [
      "hash"
      "vars"
    ];
  sources = mapAttrs (
    name: source:
    removeAttrs source [
      "url"
      "asset"
      "archive"
    ]
    // {
      url = url source;
    }
    // (if hashes ? ${name} then { hash = hashes.${name}; } else { })
  ) declared;
  # the tag act.toml's template spells for `pinned`, or the one spieler recorded for a `*`
  tagOf =
    pinned:
    let
      version = pinned.version or "";
      template = act.tag or null;
    in
    pinned.tag or (
      if template == null then
        version
      else if match ".*[*].*" template != null then
        throw "${where}: tag ${template} needs the tag spieler records in act.lock"
      else
        replaceStrings
          [ "{version}" "{version_}" ]
          [
            version
            (replaceStrings [ "." ] [ "_" ] version)
          ]
          template
    );
  isPackage = filter (key: !elem key identity) (attrNames act) != [ ];
}
