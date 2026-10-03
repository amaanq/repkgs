# An interpreter's `.env` and `.project`, attached by `extras = "python"`. Both are pyapp packages
# over fetch.pythonDeps, and the dev env's python also imports the project at $MOTLEY_PROJECT.
{
  package,
  fetch,
  pkgs,
  buildPkgs,
  python,
}:
let
  hints = (builtins.fromJSON (builtins.readFile ../builder/sys-libs.json)).python;
  ignored = [
    ".git"
    ".venv"
    "result"
    "__pycache__"
    ".direnv"
  ];

  build =
    mode:
    {
      src,
      lock ? null,
      name ? null,
      extras ? [ ],
      groups ? [ ],
      prefer ? "wheel",
      overrides ? { },
      sys ? [ ],
      tools ? [ ],
      check ? [ ],
      env ? { },
      environ ? { },
    }:
    let
      project = src + "/pyproject.toml";
      label =
        if name != null then
          name
        else if builtins.pathExists project then
          builtins.replaceStrings [ "_" "." ] [ "-" "-" ]
            (builtins.fromTOML (builtins.readFile project)).project.name
        else
          baseNameOf src;
      source = builtins.path {
        name = "${label}-src";
        path = src;
        filter = path: _: !builtins.elem (baseNameOf path) ignored;
      };

      locked =
        (builtins.fromTOML (builtins.readFile (if lock != null then lock else src + "/uv.lock"))).package
          or [ ];
      hinted = builtins.filter (hint: hints ? ${hint}) (map (entry: entry.name) locked);
      named = builtins.attrNames overrides;
      fromOverrides = field: builtins.concatMap (name: overrides.${name}.${field} or [ ]) named;

      # uv.lock pins git dependencies by commit alone, url[?query]#rev
      git = map (
        entry:
        let
          parts = builtins.match "([^?#]+)(\\?[^#]*)?#(.*)" entry.source.git;
        in
        {
          inherit (entry) name;
          path = builtins.fetchGit {
            url = builtins.head parts;
            rev = builtins.elemAt parts 2;
            allRefs = true;
          };
        }
      ) (builtins.filter (entry: entry ? source.git) locked);
    in
    package {
      name =
        {
          project = label;
          env = "${label}-env";
          dev = "${label}-dev";
        }
        .${mode};
      inherit source env;
      version = "0";
      uses = [ "pyapp" ];
      pyapp = {
        inherit mode check;
        deps = fetch.pythonDeps {
          inherit
            python
            extras
            prefer
            git
            ;
          source =
            if lock == null then
              source
            else
              builtins.path {
                name = "${label}-lock";
                path = dirOf lock;
                filter = _: _: true;
              };
          groups = if mode == "project" then [ ] else groups;
          sdist = builtins.filter (name: overrides.${name}.prefer or prefer == "sdist") named;
          environ = builtins.toJSON environ;
        };
        overrides = builtins.mapAttrs (_: over: {
          env = over.env or { };
          patches = map toString (over.patches or [ ]);
          run = over.run or "";
          configSettings = over.configSettings or { };
        }) overrides;
      };
      sys =
        sys
        ++ builtins.filter (lib: pkgs ? ${lib}) (
          builtins.concatMap (hint: if hints.${hint} ? pkg then [ hints.${hint}.pkg ] else [ ]) hinted
        )
        ++ fromOverrides "sys";
      buildDependencies = map (tool: buildPkgs.${tool}) (
        tools
        ++ builtins.filter (tool: buildPkgs ? ${tool}) (
          builtins.concatMap (hint: hints.${hint}.tools or [ ]) hinted
        )
        ++ fromOverrides "tools"
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
  env = args: build "env" ({ src = dirOf args.lock; } // args);
  project = args: {
    package = build "project" args;
    devEnv = build "dev" args;
  };
}
