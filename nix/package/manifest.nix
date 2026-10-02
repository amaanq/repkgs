# A package described by its recipe.toml and, where some of it depends on the platform or names
# other packages, a recipe.nix. Besides [upstream], [[source]] and [pin] (nix/sources.nix) the
# toml holds:
#   sources-from = "<package>"  that package's sources instead of its own, for a subproject of
#                               its tarball
#   helpers = "python"          the interpreter also carries .env and .project (nix/python.nix)
#   [build]                     recipe fields as plain data, `name` defaults to the directory
#
# recipe.nix is a function of { platform, pkgs, tools, toolchain, pin, source, file, fetch, tree,
# when } to a list of changes, each a table of the same fields merged over [build] in order.
# Tables merge, lists append, anything else is replaced. `pkgs` and `tools` are the target and
# build sets, `source <key>` one of this package's sources, `file <name>` a file beside the
# recipe, `fetch.<producer>` a nix/fetch.nix producer, `when <cond> <change>` the change or
# nothing, and `tree { dir, name, exclude, files }` a local tree beside the recipe without the
# `exclude`d names, and with `files` only the files whose name is listed or, for an entry starting
# with ".", ends in it.
#
# Field names differ from the recipe the build systems take: `systems` is `uses`, `tools`
# `buildDependencies`, `needs` `dependencies`, `targets` `platforms`, `after`, `before`, `skip` and
# `replace` the `phases` table (`phases` itself where it lists every phase), and `check` is
# `tests`, false for `tests.run = false`. Entries of `needs` and `check.needs` are target packages
# by name, of `tools` build packages, and `patches` are files beside the recipe.
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
    attrNames
    concatStringsSep
    elem
    filter
    foldl'
    isAttrs
    isBool
    isList
    isString
    mapAttrs
    pathExists
    removeAttrs
    ;
  where = "${name}: recipe.toml";
  known = [
    "upstream"
    "source"
    "pin"
    "watch"
    "locks"
    "build"
    "sources-from"
    "helpers"
  ];
  strays = filter (key: !elem key known) (attrNames table);

  merge =
    base: top:
    if isAttrs base && isAttrs top then
      base // mapAttrs (key: value: if base ? ${key} then merge base.${key} value else value) top
    else if isList base && isList top then
      base ++ top
    else
      top;

  renamed = {
    systems = "uses";
    tools = "buildDependencies";
    needs = "dependencies";
    targets = "platforms";
  };
  phaseFields = {
    after = "after";
    before = "before";
    skip = "remove";
    replace = "replace";
  };
  checks =
    value:
    if isBool value then
      { run = value; }
    else
      removeAttrs value [ "needs" ] // (if value ? needs then { dependencies = value.needs; } else { });
  internal =
    change:
    let
      phased = filter (key: change ? ${key}) (attrNames phaseFields);
      plain = removeAttrs change (attrNames renamed ++ attrNames phaseFields ++ [ "check" ]);
    in
    plain
    // builtins.listToAttrs (
      map (key: {
        name = renamed.${key};
        value = change.${key};
      }) (filter (key: change ? ${key}) (attrNames renamed))
    )
    // (if change ? check then { tests = checks change.check; } else { })
    // (
      if phased == [ ] then
        { }
      else
        {
          phases =
            (change.phases or { })
            // builtins.listToAttrs (
              map (key: {
                name = phaseFields.${key};
                value = change.${key};
              }) phased
            );
        }
    );

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
    if pathExists (dir + "/recipe.nix") then
      import (dir + "/recipe.nix") {
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
  fields = foldl' merge (internal (table.build or { })) (map internal changes);

  byName =
    set: entry:
    if isString entry then set.${entry} or (throw "${where}: no package ${entry}") else entry;
  resolved =
    fields
    // {
      name = fields.name or name;
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
    // (if fields ? patches then { patches = map (file: dir + "/${file}") fields.patches; } else { });
  inherit (builtins.fromJSON (builtins.readFile ../schema.json)) systems;
  # a build system's table is emptied where `uses` does not list the system
  made = package (
    resolved
    // builtins.listToAttrs (
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
    if !(table ? helpers) then
      made
    else
      made // helpers.${table.helpers} or (throw "${where}: no helpers ${table.helpers}");
in
if strays != [ ] then throw "${where}: unknown table ${concatStringsSep ", " strays}" else self
