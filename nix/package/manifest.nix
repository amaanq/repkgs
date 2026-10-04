# A package described by its act.toml (docs/act.md) and, where some of it needs Nix, an act.nix.
# The identity keys (`name`, `tag`, `track`, `fetch`) are nix/act.nix's, the rest is the build:
# renamed onto the fields bigtop reads, `[on.<when>]` overlays merged in name order on matching
# platforms, then act.nix's changes in order. Tables merge, lists append, anything else replaces.
#
# act.nix is a function of { platform, pkgs, tools, toolchain, pin, source, file, fetch, tree,
# when } to a list of changes in act.toml's vocabulary. `pkgs` and `tools` are the target and
# build sets, `source <key>` one of this package's sources, `file <name>` a file beside the act,
# `fetch.<producer>` a nix/fetch.nix producer, `when <cond> <change>` the change or nothing, and
# `tree { dir, name, exclude, files }` a local tree beside the act without the `exclude`d names,
# and with `files` only the files whose name is listed or, for an entry starting with ".", ends
# in it.
{
  dir,
  name,
  table,
}:
{
  package,
  pkgs,
  buildPkgs,
  platform,
  sources,
  toolchain,
  fetch,
  pythonProject,
}:
let
  inherit (builtins)
    all
    attrNames
    concatMap
    concatStringsSep
    elem
    filter
    foldl'
    isAttrs
    isBool
    isList
    isString
    length
    listToAttrs
    mapAttrs
    match
    pathExists
    readDir
    readFile
    replaceStrings
    split
    substring
    ;
  where = "${name}: act.toml";

  merge =
    base: top:
    if isAttrs base && isAttrs top then
      base // mapAttrs (key: value: if base ? ${key} then merge base.${key} value else value) top
    else if isList base && isList top then
      base ++ top
    else
      top;

  renamed = {
    "with" = "uses";
    tools = "buildDependencies";
    deps = "dependencies";
    only = "platforms";
  };
  stepFields = {
    after = "after";
    before = "before";
    skip = "remove";
    replace = "replace";
  };
  tests =
    value:
    if isBool value then
      { run = value; }
    else
      removeAttrs value [
        "deps"
        "bin"
      ]
      // (if value ? deps then { dependencies = value.deps; } else { });
  internal =
    change:
    let
      steps = change.steps or { };
      stepped = filter (key: steps ? ${key}) (attrNames stepFields);
      test = change.test or { };
    in
    removeAttrs change (
      attrNames renamed
      ++ [
        "test"
        "steps"
      ]
    )
    // listToAttrs (
      map (key: {
        name = renamed.${key};
        value = change.${key};
      }) (filter (key: change ? ${key}) (attrNames renamed))
    )
    // (
      if change ? test && (isBool test || removeAttrs test [ "bin" ] != { }) then
        { tests = tests test; }
      else
        { }
    )
    // (if isAttrs test && test ? bin then { inherit (test) bin; } else { })
    // (
      if stepped != [ ] then
        {
          phases = listToAttrs (
            map (key: {
              name = stepFields.${key};
              value = steps.${key};
            }) stepped
          );
        }
      else if steps ? phases then
        { inherit (steps) phases; }
      else
        { }
    );

  # `[on."windows+msvc"]`, `[on."!linux"]`, `[on.cross]`
  holds =
    term:
    let
      negated = substring 0 1 term == "!";
      word = if negated then substring 1 (-1) term else term;
      value =
        if word == "cross" then
          platform.cross
        else if word == "native" then
          !platform.cross
        else
          elem word [
            platform.os
            platform.cpu
            platform.libc
            platform.abi
          ];
    in
    value != negated;
  matching = when: all holds (filter isString (split "[+]" when));
  overlays = map (when: table.on.${when}) (filter matching (attrNames (table.on or { })));

  patchFile =
    entry:
    if match "(https://|github:|gitlab:).*" entry == null then
      entry
    else
      vendored.${entry}
        or (throw "${where}: patch ${entry} is not vendored, run `spieler patch ${name}`");
  # a patch beside the act applies only where act.toml names it, so one left out is a mistake
  inherit ((import ../act.nix dir)) vendored;
  named = map (entry: vendored.${entry} or entry) (
    (table.patches or [ ])
    ++ concatMap (when: table.on.${when}.patches or [ ]) (attrNames (table.on or { }))
  );
  # act.nix can hand a patch to something else, perl's PERL_CROSS_PATCH or a cratePatches entry
  nixText = if pathExists (dir + "/act.nix") then readFile (dir + "/act.nix") else "";
  mentioned = file: length (split (replaceStrings [ "." "+" ] [ "[.]" "[+]" ] file) nixText) > 1;
  unlisted = filter (
    file: match ".*[.](patch|diff)" file != null && !elem file named && !mentioned file
  ) (attrNames (readDir dir));
  base = removeAttrs table [
    "name"
    "tag"
    "track"
    "fetch"
    "on"
    "extras"
  ];

  tree =
    spec:
    let
      exclude = spec.exclude or [ ];
      wanted =
        file:
        builtins.any (
          entry:
          if builtins.substring 0 1 entry == "." then
            builtins.match ".*[.]${builtins.substring 1 (-1) entry}" file != null
          else
            file == entry
        ) spec.files;
    in
    builtins.path {
      path = dir + "/${spec.dir}";
      inherit (spec) name;
      filter =
        path: type:
        let
          base = baseNameOf path;
        in
        !elem base exclude && (type == "directory" || !(spec ? files) || wanted base);
    };
  changes =
    if pathExists (dir + "/act.nix") then
      import (dir + "/act.nix") {
        inherit
          platform
          pkgs
          toolchain
          fetch
          tree
          ;
        tools = buildPkgs;
        pin = { inherit (sources) version tag; };
        source = sources.fetch;
        file = file: dir + "/${file}";
        when = holds: change: if holds then change else { };
      }
    else
      [ ];
  expand =
    value:
    if sources == null then
      value
    else if isString value then
      sources.expand value
    else if isList value then
      map expand value
    else if isAttrs value then
      mapAttrs (_: expand) value
    else
      value;
  fields = foldl' merge (internal (expand base)) (map internal (map expand overlays ++ changes));

  byName =
    set: entry:
    if isString entry then set.${entry} or (throw "${where}: no package ${entry}") else entry;
  inherit (builtins.fromJSON (builtins.readFile ../schema.json)) systems;
  resolved =
    fields
    // {
      name = table.name or name;
    }
    // (
      if fields ? dependencies then { dependencies = map (byName pkgs) fields.dependencies; } else { }
    )
    // (
      if fields ? buildDependencies then
        { buildDependencies = map (byName buildPkgs) fields.buildDependencies; }
      else
        { }
    )
    // (
      if fields ? tests && fields.tests ? dependencies then
        {
          tests = fields.tests // {
            dependencies = map (byName pkgs) fields.tests.dependencies;
          };
        }
      else
        { }
    )
    // (
      if unlisted != [ ] then
        {
          patches = throw "${where}: ${concatStringsSep " " unlisted} sit beside the act but patches does not name them";
        }
      else if fields ? patches then
        { patches = map (entry: dir + "/${patchFile entry}") fields.patches; }
      else
        { }
    )
    # a system's `tool` may name a package of the build set
    // listToAttrs (
      map
        (system: {
          name = system;
          value = fields.${system} // {
            tool = byName buildPkgs fields.${system}.tool;
          };
        })
        (filter (system: isAttrs (fields.${system} or null) && fields.${system} ? tool) (attrNames systems))
    );
  # a build system's table is emptied where `uses` does not list the system
  made = package (
    resolved
    // listToAttrs (
      map
        (system: {
          name = system;
          value = { };
        })
        (filter (system: resolved ? ${system} && !elem system (resolved.uses or [ ])) (attrNames systems))
    )
  );
  helpers = {
    python = pythonProject package self;
  };
  self =
    if !(table ? extras) then
      made
    else
      made // helpers.${table.extras} or (throw "${where}: no extras ${table.extras}");
in
self
