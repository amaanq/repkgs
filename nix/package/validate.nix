# A resolved spec against the schema bigtop emits: `errors` lists what is wrong at the top, `valid` is
# the spec with each field checked deeply where it is read
{
  platform,
  buildSystems,
  schema,
  lib,
}:
{
  args,
  uses,
  fail,
}:
let
  inherit (builtins)
    all
    attrNames
    concatMap
    elem
    filter
    isList
    mapAttrs
    ;

  # `<bs> = on cond { … }` for a build system used on some platforms only leaves { } on the others
  inactive = filter (key: !(elem key schema.recipeFields) && args.${key} == { }) (attrNames args);
  fields = schema.validateRecipe {
    value = removeAttrs args (uses ++ inactive);
    mode = "keys";
  };
  options = concatMap (
    system:
    buildSystems.${system}.validate {
      value = args.${system} or { };
      mode = "keys";
    }
  ) uses;
  # the shape is the schema's, values from outside are checked where they are resolved (nix/features.nix)
  features = map (feature: "features ${feature}: the default is not among the values") (
    filter (
      feature:
      let
        declared = args.features.${feature};
        defaults = if isList declared.default then declared.default else [ declared.default ];
      in
      declared ? values && !all (value: elem value declared.values) defaults
    ) (attrNames (args.features or { }))
  );
  # a library among the build tools or a tool among the libraries: natively both platforms
  # coincide and nothing would notice, so it is checked here
  misplaced =
    field: expected: dependencies:
    map (dependency: "${field}: ${dependency.pname} is built for ${dependency.platform}") (
      filter (dependency: (dependency.platform or expected) != expected) dependencies
    );
in
{
  errors = [
    fields
    features
    options
    (misplaced "dependencies" platform.name (args.dependencies or [ ]))
    (misplaced "buildDependencies" platform.system (args.buildDependencies or [ ]))
  ];
  # fully checked only where read, since an interpolated string forces its dependency
  valid = mapAttrs (
    field: value:
    let
      errors = schema.checkField "deep" field value;
    in
    if !(elem field schema.recipeFields) || errors == [ ] then value else fail (lib.join "; " errors)
  ) args;
}
