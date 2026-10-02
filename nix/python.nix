# What `helpers = "python"` in a package.toml adds to the interpreter:
#   cpython.env { src | lock, … }     an environment: every locked dependency plus bin/python3
#   cpython.project { src, … }        { package = the application; devEnv = an env whose python
#                                     also imports the checkout named by $REPKGS_PROJECT }
# Both are pyapp packages around fetch.pythonDeps. Example: tests/builder/pyproject.
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
  pypiName = replaceStrings [ "_" "." ] [ "-" "-" ];
  # per PyPI name: C libraries to link, build tools, config settings (builder/sys-libs.nu)
  known = (fromJSON (readFile ../builder/sys-libs.json)).python;

  # git sources: uv.lock has url?rev#sha and no content hash, so they are fetched here by commit
  gitSources = concatMap (
    locked:
    let
      parts = match "([^?#]+)(\\?[^#]*)?#(.*)" (locked.source.git or "");
    in
    if locked ? source.git then
      [
        {
          inherit (locked) name;
          path = builtins.fetchGit {
            url = head parts;
            rev = elemAt parts 2;
            allRefs = true;
          };
        }
      ]
    else
      [ ]
  );

  application =
    mode:
    {
      src, # the project (its pyproject.toml names it); with `lock`, only used for the name
      lock ? null, # a uv.lock elsewhere than src/uv.lock
      name ? null,
      extras ? [ ],
      groups ? [ ], # PEP 735 dependency groups, env and dev only
      prefer ? "wheel", # or "sdist", for every package the lock has both for
      # per locked package, by PyPI name: { prefer, sys, tools, patches, env, run (nu, in its
      # unpacked source), configSettings }
      overrides ? { },
      sys ? [ ], # extra C libraries by package name, on top of sys-libs.json
      tools ? [ ], # extra build tools by package name
      check ? [ ], # modules that must import from the result
      env ? { },
      environ ? { }, # PEP 508 marker overrides, e.g. { platform_release = "6.6"; }
    }:
    let
      manifest = src + "/pyproject.toml";
      pname =
        if name != null then
          name
        else if pathExists manifest then
          pypiName (fromTOML (readFile manifest)).project.name
        else
          baseNameOf src;
      source = builtins.path {
        path = src;
        name = "${pname}-src";
        filter =
          path: _type:
          !(elem (baseNameOf path) [
            ".git"
            ".venv"
            "result"
            "__pycache__"
            ".direnv"
          ]);
      };
      lockData = fromTOML (readFile (if lock == null then src + "/uv.lock" else lock));
      lockRoot =
        if lock == null then
          source
        else
          builtins.path {
            path = dirOf lock;
            name = "${pname}-lock";
          };
      overridden = attrNames overrides;
      fromOverrides = field: concatMap (entry: overrides.${entry}.${field} or [ ]) overridden;
      lockedKnown = filter (entry: known ? ${entry}) (
        map (locked: locked.name) (lockData.package or [ ])
      );
      fromKnown = field: concatMap (entry: known.${entry}.${field} or [ ]) lockedKnown;
      knownLibraries = filter (library: pkgs ? ${library}) (
        concatMap (entry: if known.${entry} ? pkg then [ known.${entry}.pkg ] else [ ]) lockedKnown
      );
    in
    package {
      name = if mode == "project" then pname else "${pname}-${mode}";
      inherit source env;
      version = "0";
      uses = [ "pyapp" ];
      pyapp = {
        inherit mode check;
        deps = fetch.pythonDeps {
          inherit python extras prefer;
          source = lockRoot;
          groups = if mode == "project" then [ ] else groups;
          sdist = filter (entry: overrides.${entry}.prefer or prefer == "sdist") overridden;
          git = gitSources (lockData.package or [ ]);
          environ = toJSON environ;
        };
        overrides = mapAttrs (_entry: override: {
          env = override.env or { };
          patches = map toString (override.patches or [ ]);
          run = override.run or "";
          configSettings = override.configSettings or { };
        }) overrides;
      };
      sys = sys ++ knownLibraries ++ fromOverrides "sys";
      buildDependencies = map (tool: buildPkgs.${tool}) (
        tools ++ filter (tool: buildPkgs ? ${tool}) (fromKnown "tools") ++ fromOverrides "tools"
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
  env = args: application "env" (if args ? src then args else args // { src = dirOf args.lock; });
  project = args: {
    package = application "project" args;
    devEnv = application "dev" args;
  };
}
