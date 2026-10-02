# Calling a package.nix: its recipe.toml read when the directory has one, its features resolved,
# and `package`/`variant` bound to this name
{
  scope,
  package,
  readSources,
  overrides,
  overrideTree,
  features,
  self,
}:
let
  inherit (import ../features.nix) resolve;
  # one package.nix evaluated under `name`: `fn` its function, `edit0` what `variant` adds on
  # top of the spec (identity for the package itself). `variant pkgs.llvm { … }` re-enters here
  # with llvm's function, so the base body sees this name's sources and features
  callWith =
    fn: dirs: name: sources: edit0:
    let
      formals = builtins.functionArgs fn;
      edit = if edit0 == null then (spec: spec) else edit0;
      local = overrideTree.${name}.features or { };
      given = extra: builtins.intersectAttrs formals (scope // { inherit sources; } // extra);
      # default <- set-wide <- overrides.<name>.features, for a package.nix that takes `features`.
      # Its declaration is read by one more call whose `package` just returns it: lazy (no spec
      # is built), and the real call's dependencies may then depend on feature values. One that
      # declares features without reading them only pays when overridden
      declared =
        (fn (given {
          features = { };
          package = spec: { decl = (edit spec).features or { }; };
          variant = base: _tree: { inherit (base) decl; };
        })).decl;
      resolved = if formals ? features || local != { } then resolve name declared features local else { };
    in
    fn (given {
      features = resolved;
      package =
        if edit0 == null then
          package sources dirs resolved fn
        else
          spec: package sources dirs resolved fn (edit0 spec);
      # another package's spec under this name, edited with override verbs: this function
      # again with the base's package.nix. Own recipe.toml when the directory has one
      # (llvm22: another pin), else the base's (rust-std: same tarball). A list of trees merges
      variant =
        base: tree:
        callWith base.fn base.dirs name (if sources == null then base.sources else sources) (
          spec: overrides.applyOne self name (overrides.merge tree) (edit spec // { inherit name; })
        );
    });
in
dir: name:
let
  file = dir + "/recipe.toml";
  recipe = dir + "/package.nix";
  table = if builtins.pathExists file then builtins.fromTOML (builtins.readFile file) else { };
  fn =
    if builtins.pathExists recipe then
      import recipe
    else
      import ../package/manifest.nix { inherit dir name table; };
  # `sources-from = "<package>"`: the sources of that package, a subproject of its tarball
  lender = ../../pkgs + "/${builtins.substring 0 2 table."sources-from"}/${table."sources-from"}";
  sources =
    if table ? "sources-from" then
      readSources (lender + "/recipe.toml")
    else if table ? source || table ? upstream then
      readSources file
    else
      null;
in
# a phase module not beside recipe.toml may be the lender's
callWith fn ([ dir ] ++ (if table ? "sources-from" then [ lender ] else [ ])) name sources null
