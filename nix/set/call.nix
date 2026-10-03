# Calling a package.nix: its act.toml read when the directory has one, its features resolved,
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
        let
          build = package {
            inherit sources dirs fn;
            features = resolved;
          };
        in
        if edit0 == null then build else spec: build (edit0 spec);
      # another package's spec under this name, edited with override verbs: this function
      # again with the base's package.nix. Own act.toml when the directory has one
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
  recipe = dir + "/package.nix";
  act = if builtins.pathExists (dir + "/act.toml") then import ../act.nix dir else null;
  table = if act == null then { } else act.act;
  fn =
    if builtins.pathExists recipe then
      import recipe
    else
      import ../package/manifest.nix { inherit dir name table; };
  # `fetch.from = "<package>"`: the sources of that package, a subproject of its tarball
  lender = ../../pkgs + "/${builtins.substring 0 2 act.from}/${act.from}";
  borrows = act != null && act.from != null;
  sources =
    if borrows then
      readSources lender
    else if act != null && (act.sources != { } || act.pinned != { } || table ? track) then
      readSources dir
    else
      null;
in
# a phase module not beside act.toml may be the lender's
callWith fn ([ dir ] ++ (if borrows then [ lender ] else [ ])) name sources null
