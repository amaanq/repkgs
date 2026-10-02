# A package described by its package.toml alone, no package.nix. Besides [upstream], [[source]]
# and [pin] (nix/sources.nix) the file holds:
#   from = "<package>"   that package's sources instead of its own, for a subproject of its tarball
#   helpers = "python"   the interpreter also carries .env and .project (nix/python.nix)
#   [package]           recipe fields, `name` defaults to the directory
#   [vars]              named strings for {var:…}
#   [when.<condition>]  merged on top where the condition holds, [unless.<condition>] where not
# Conditions are `cross`, `native`, `os.<os>`, `cpu.<cpu>`, `libc.<libc>` and `binfmt.<binfmt>`,
# applied in that order. Tables merge, lists append, anything else is replaced.
#
# Strings may name things: {pkg:<name>} and {tool:<name>} are a package of the target and the
# build set, {src:<name>} a package's source, {source:<key>} one of this package's sources,
# {file:<name>} a file beside package.toml, {platform.<path>} a platform fact (`march.0`),
# {toolchain.sysroot} the sysroot, {pin.version} and {pin.tag} the pinned release, {var:<name>} a
# var. A string that is only one of these is that value itself, a package, a source tree or a
# path. "{null}" is null, which TOML cannot spell.
#
# Two tables stand for values: { fetch = "<producer>", … } is that nix/fetch.nix producer called
# with the other keys, { tree = "<dir>", name, exclude, files } a local tree beside package.toml
# without the `exclude`d names, and with `files` only the files whose name is listed or, for an
# entry starting with ".", ends in it.
#
# Entries of `dependencies` and `tests.dependencies` are target packages by name, entries of
# `buildDependencies` build packages, and `patches` are files beside package.toml.
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
    head
    isAttrs
    isList
    isString
    length
    mapAttrs
    split
    ;
  where = "${name}: package.toml";
  known = [
    "upstream"
    "source"
    "pin"
    "watch"
    "locks"
    "package"
    "vars"
    "when"
    "unless"
    "from"
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

  facts = {
    inherit (platform)
      os
      cpu
      libc
      binfmt
      ;
  };
  inherit (builtins.fromJSON (builtins.readFile ../schema.json)) targets systems;
  values = kind: builtins.catAttrs kind targets;
  # the tables of one [when] or [unless] block whose condition matches `holds`
  selected =
    block: holds:
    let
      flag =
        key:
        if block ? ${key} && holds (if key == "cross" then platform.cross else !platform.cross) then
          [ block.${key} ]
        else
          [ ];
      keyed =
        kind:
        let
          options = block.${kind} or { };
          unknown = filter (value: !elem value (values kind)) (attrNames options);
        in
        if unknown != [ ] then
          throw "${where}: no ${kind} ${concatStringsSep ", " unknown}"
        else
          map (value: options.${value}) (filter (value: holds (value == facts.${kind})) (attrNames options));
      strayConditions = filter (
        key:
        !elem key [
          "cross"
          "native"
          "os"
          "cpu"
          "libc"
          "binfmt"
        ]
      ) (attrNames block);
    in
    if strayConditions != [ ] then
      throw "${where}: no condition ${concatStringsSep ", " strayConditions}"
    else
      flag "native" ++ flag "cross" ++ keyed "os" ++ keyed "cpu" ++ keyed "libc" ++ keyed "binfmt";

  layers =
    selected (table.when or { }) (holds: holds) ++ selected (table.unless or { }) (holds: !holds);
  merged =
    foldl' merge
      {
        package = table.package or { };
        vars = table.vars or { };
      }
      (
        map (layer: {
          package = removeAttrs layer [ "vars" ];
          vars = layer.vars or { };
        }) layers
      );

  lookup =
    kind: key:
    if kind == "pkg" then
      pkgs.${key} or (throw "${where}: no package ${key}")
    else if kind == "tool" then
      buildPkgs.${key} or (throw "${where}: no package ${key}")
    else if kind == "source" then
      sources.fetch key
    else if kind == "src" then
      (pkgs.${key} or (throw "${where}: no package ${key}")).src
    else if kind == "file" then
      dir + "/${key}"
    else if kind == "var" then
      merged.vars.${key} or (throw "${where}: no var ${key}")
    else
      let
        found =
          builtins.foldl'
            (
              value: field:
              if isAttrs value && value ? ${field} then
                value.${field}
              else if
                isList value && builtins.match "[0-9]+" field != null && builtins.fromJSON field < length value
              then
                builtins.elemAt value (builtins.fromJSON field)
              else
                null
            )
            (
              if kind == "toolchain" then
                toolchain
              else if kind == "pin" then
                { inherit (sources) version tag; }
              else
                platform
            )
            (filter isString (split "[.]" key));
      in
      if found == null then throw "${where}: no ${kind} fact ${key}" else found;
  pattern = "[{](pkg|tool|src|source|file|var):([^}]+)[}]|[{](platform|toolchain|pin)[.]([A-Za-z0-9.]+)[}]";
  capture =
    groups:
    if head groups != null then
      lookup (head groups) (builtins.elemAt groups 1)
    else
      lookup (builtins.elemAt groups 2) (builtins.elemAt groups 3);
  expand =
    text:
    let
      parts = split pattern text;
    in
    if text == "{null}" then
      null
    else if length parts == 3 && head parts == "" && builtins.elemAt parts 2 == "" then
      capture (builtins.elemAt parts 1)
    else
      concatStringsSep "" (map (part: if isList part then toString (capture part) else part) parts);
  deep =
    value:
    if isString value then
      expand value
    else if isList value then
      map deep value
    else if isAttrs value && value ? fetch then
      fetch.${value.fetch} (deep (removeAttrs value [ "fetch" ]))
    else if isAttrs value && value ? tree then
      tree value
    else if isAttrs value then
      mapAttrs (_: deep) value
    else
      value;
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
      path = dir + "/${spec.tree}";
      inherit (spec) name;
      filter =
        path: type:
        let
          base = baseNameOf path;
        in
        !elem base exclude && (type == "directory" || !(spec ? files) || wanted base);
    };

  byName =
    set: entry:
    if isString entry then set.${entry} or (throw "${where}: no package ${entry}") else entry;
  fields = deep merged.package;
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
