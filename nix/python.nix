# The interpreter's `.env` and `.project`, attached by `helpers = "python"` in its recipe.toml.
# Both build pyapp packages over fetch.pythonDeps (tests/builder/pyproject):
#   .env { src or lock, … }   every locked dependency and bin/python3
#   .project { src, … }       { package, devEnv }, where devEnv's python also imports the checkout
#                             $MOTLEY_PROJECT names
{
  package,
  fetch,
  pkgs,
  buildPkgs,
  python,
}:
let
  inherit (builtins)
    attrNames
    baseNameOf
    concatMap
    dirOf
    elem
    elemAt
    filter
    fromJSON
    fromTOML
    head
    mapAttrs
    match
    pathExists
    readFile
    replaceStrings
    toJSON
    ;
  # libraries, tools and settings PyPI packages need, by name (builder/sys-libs.nu)
  hints = (fromJSON (readFile ../builder/sys-libs.json)).python;

  copy =
    path: name: filter:
    builtins.path { inherit path name filter; };
  everything = _: _: true;
  checkout =
    path: _:
    !elem (baseNameOf path) [
      ".git"
      ".venv"
      "result"
      "__pycache__"
      ".direnv"
    ];

  titleOf =
    src: name:
    let
      pyproject = src + "/pyproject.toml";
    in
    if name != null then
      name
    else if pathExists pyproject then
      replaceStrings [ "_" "." ] [ "-" "-" ] (fromTOML (readFile pyproject)).project.name
    else
      baseNameOf src;

  # uv.lock gives a git dependency as url?query#commit with no content hash
  pinnedGit =
    entry:
    let
      parts = match "([^?#]+)(\\?[^#]*)?#(.*)" entry.source.git;
    in
    {
      inherit (entry) name;
      path = builtins.fetchGit {
        url = head parts;
        rev = elemAt parts 2;
        allRefs = true;
      };
    };

  build =
    mode:
    {
      src,
      # a uv.lock outside src, which then only names the package
      lock ? null,
      name ? null,
      extras ? [ ],
      # PEP 735 groups, for env and dev
      groups ? [ ],
      # "wheel" or "sdist", wherever the lock offers both
      prefer ? "wheel",
      # by PyPI name, { prefer, sys, tools, patches, env, run, configSettings } where run is nu
      # run in the unpacked source
      overrides ? { },
      sys ? [ ],
      tools ? [ ],
      # modules the result must import
      check ? [ ],
      env ? { },
      # PEP 508 marker values to assume, { platform_release = "6.6"; }
      environ ? { },
    }:
    let
      title = titleOf src name;
      tree = copy src "${title}-src" checkout;
      entries = (fromTOML (readFile (if lock == null then src + "/uv.lock" else lock))).package or [ ];
      tuned = attrNames overrides;
      hinted = filter (entry: hints ? ${entry}) (map (entry: entry.name) entries);
      byOverride = field: concatMap (entry: overrides.${entry}.${field} or [ ]) tuned;
      byHint = field: concatMap (entry: hints.${entry}.${field} or [ ]) hinted;
      libraries = filter (library: pkgs ? ${library}) (
        map (entry: hints.${entry}.pkg) (filter (entry: hints.${entry} ? pkg) hinted)
      );
    in
    package {
      name = if mode == "project" then title else "${title}-${mode}";
      source = tree;
      inherit env;
      version = "0";
      uses = [ "pyapp" ];
      pyapp = {
        inherit mode check;
        deps = fetch.pythonDeps {
          inherit python extras prefer;
          source = if lock == null then tree else copy (dirOf lock) "${title}-lock" everything;
          groups = if mode == "project" then [ ] else groups;
          sdist = filter (entry: overrides.${entry}.prefer or prefer == "sdist") tuned;
          git = map pinnedGit (filter (entry: entry ? source.git) entries);
          environ = toJSON environ;
        };
        overrides = mapAttrs (_: override: {
          env = override.env or { };
          patches = map toString (override.patches or [ ]);
          run = override.run or "";
          configSettings = override.configSettings or { };
        }) overrides;
      };
      sys = sys ++ libraries ++ byOverride "sys";
      buildDependencies = map (tool: buildPkgs.${tool}) (
        tools ++ filter (tool: buildPkgs ? ${tool}) (byHint "tools") ++ byOverride "tools"
      );
      dependencies = [
        python
        pkgs.libgcc-shim
      ];
      tests.version = false;
      exports = false;
    };
in
{
  env = args: build "env" (if args ? src then args else args // { src = dirOf args.lock; });
  project = args: {
    package = build "project" args;
    devEnv = build "dev" args;
  };
}
