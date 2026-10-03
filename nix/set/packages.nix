# pkgs/<first two letters>/<name>/ with a package.nix, or an act.toml with build keys,
# attribute name == directory name, then the
# out-of-tree `packages`, then the unversioned aliases of pkgs/aliases.toml
{
  call,
  packages,
  self,
}:
let
  inherit (builtins)
    attrNames
    concatMap
    filter
    listToAttrs
    mapAttrs
    pathExists
    readDir
    substring
    ;
  root = ../../pkgs;
  shards = attrNames (removeAttrs (readDir root) [ "aliases.toml" ]);
  inShard =
    shard:
    let
      dir = root + "/${shard}";
    in
    map (name: {
      inherit name;
      value = call (dir + "/${name}") name;
    }) (filter (name: isPackage (dir + "/${name}")) (attrNames (readDir dir)));
  isPackage =
    dir:
    pathExists (dir + "/package.nix")
    || pathExists (dir + "/act.toml") && (import ../act.nix dir).isPackage;
  # unversioned names for the default line of multi-version packages (pkgs/aliases.toml)
  aliases = mapAttrs (
    alias: target:
    if isPackage (root + "/${substring 0 2 alias}/${alias}") then
      throw "alias ${alias} shadows a package directory"
    else
      self.${target}
  ) (builtins.fromTOML (builtins.readFile (root + "/aliases.toml")));
in
{
  inherit aliases;
  all =
    listToAttrs (concatMap inShard shards) // mapAttrs (name: dir: call dir name) packages // aliases;
}
