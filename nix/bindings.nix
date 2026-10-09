# The `[bindings]` and `[builds]` of every act.toml (docs/act.md), gathered by package
let
  inherit (builtins)
    attrNames
    concatMap
    filter
    isList
    listToAttrs
    mapAttrs
    pathExists
    readDir
    readFile
    ;
  root = ../pkgs;
  acts = concatMap (
    shard:
    let
      dir = root + "/${shard}";
    in
    map (name: {
      inherit name;
      value = fromTOML (readFile (dir + "/${name}/act.toml"));
    }) (filter (name: pathExists (dir + "/${name}/act.toml")) (attrNames (readDir dir)))
  ) (attrNames (removeAttrs (readDir root) [ "aliases.toml" ]));

  spelled =
    binding:
    if isList binding then
      { names = binding; }
    else
      binding
      // {
        names =
          (binding.names or [ ])
          ++ attrNames (binding.flags or { })
          ++ attrNames (binding.configSettings or { });
      };
  section =
    key:
    listToAttrs (
      map (act: {
        inherit (act) name;
        value = mapAttrs (_: spelled) act.value.${key};
      }) (filter (act: act.value ? ${key}) acts)
    );
  bindings = section "bindings";
in
{
  inherit bindings;
  builds = section "builds";
  locked =
    ecosystem:
    listToAttrs (
      concatMap (
        pkg:
        map (name: {
          inherit name;
          value = pkg;
        }) (bindings.${pkg}.${ecosystem}.names or [ ])
      ) (attrNames bindings)
    );
}
