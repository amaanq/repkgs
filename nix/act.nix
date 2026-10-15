# A package directory's act.toml and act.lock (docs/act.md) as the loaders use them: the pinned
# values, the tag, and every source by name with its url template and hash.
dir:
let
  inherit (builtins)
    attrNames
    concatStringsSep
    elem
    filter
    fromTOML
    genList
    head
    isAttrs
    isString
    mapAttrs
    match
    pathExists
    readFile
    removeAttrs
    replaceStrings
    split
    substring
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
  project = match "pypi:([^?]+)" (if track == null then "" else track);
  # PEP 625's sdist name, lowercase with each run of `-`, `_` and `.` as one `_`
  normalized =
    let
      letters = upper: genList (index: substring index 1 upper) 26;
      lower = replaceStrings (letters "ABCDEFGHIJKLMNOPQRSTUVWXYZ") (
        letters "abcdefghijklmnopqrstuvwxyz"
      );
      name = if project == null then throw "${where}: sdist needs a pypi track" else head project;
    in
    lower (concatStringsSep "_" (filter isString (split "[-_.]+" name)));
  url =
    source:
    source.url or (
      if source ? sdist then
        "https://files.pythonhosted.org/packages/source/${substring 0 1 normalized}/${normalized}/${
          if isString source.sdist then
            source.sdist
          else if source.sdist then
            normalized
          else
            throw "${where}: sdist = false names no source"
        }-{version}.tar.gz"
      else if source ? asset then
        "${forge}/releases/download/{tag}/${source.asset}"
      else if source.archive or false then
        "${forge}/archive/refs/tags/{tag}.tar.gz"
      else
        throw "${where}: a source needs url, asset, archive or sdist"
    );

  single =
    act ? fetch && (act.fetch ? url || act.fetch ? asset || act.fetch ? archive || act.fetch ? sdist);
  from = act.fetch.from or null;
  declared =
    if single then
      { default = act.fetch; }
    else if act ? fetch && from == null then
      act.fetch
    else
      { };
  # what a `sources` hook named for the pin, after act.toml's
  generated = lock.sources or { };
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
      "patches"
      "sources"
    ];
  # remote patch -> the file spieler vendored it as
  vendored = lock.patches or { };
  sources = mapAttrs (
    name: source:
    removeAttrs source [
      "url"
      "asset"
      "archive"
      "sdist"
    ]
    // {
      url = url source;
    }
    // (if hashes ? ${name} then { hash = hashes.${name}; } else { })
  ) (declared // generated);
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
