# Recipe edits from outside the tree, the `overrides` argument (docs/design.md, Overrides). Keys
# lead from a package name down a field path to a node of verbs, `edit` on a package runs last.
let
  inherit (builtins)
    all
    any
    attrNames
    concatMap
    concatStringsSep
    elem
    filter
    isAttrs
    isFunction
    isList
    isString
    length
    removeAttrs
    typeOf
    zipAttrsWith
    ;

  verbs = [
    "set"
    "append"
    "prepend"
    "merge"
    "remove"
  ];
  isVerb = key: elem key verbs;
  isVerbs = node: isAttrs node && attrNames node != [ ] && all isVerb (attrNames node);
  words = concatStringsSep " ";
  nameOf = package: package.pname or package.name or (toString package);

  join = trees: zipAttrsWith joinKey trees;
  joinKey =
    key: values:
    if isVerb key && all isVerbs values then
      join values
    else if key == "set" || key == "edit" then
      builtins.elemAt values (length values - 1)
    else if key == "features" || key == "merge" then
      builtins.foldl' (into: next: into // next) { } values
    # dropping the whole field wins over dropping items from it
    else if key == "remove" && any (value: value == true) values then
      true
    else if
      elem key [
        "remove"
        "append"
        "prepend"
      ]
    then
      concatMap (
        value: if isList value then value else throw "overrides: ${key} takes a list, got a ${typeOf value}"
      ) values
    else if all isAttrs values then
      join values
    else
      throw "overrides: conflicting values at ${key}";

  at = scope: path: "overrides: ${scope.name}.${concatStringsSep "." path}";

  descend =
    scope: path: tree: present: current:
    let
      here = at scope path;
      base =
        if !present then
          { }
        else if isAttrs current then
          current
        else
          throw "${here}: is a ${typeOf current}, cannot descend";
      keys = attrNames tree;
      verbal = filter (key: isVerb key && !isVerbs tree.${key}) keys;
      children = builtins.mapAttrs (
        key: sub: descend scope (path ++ [ key ]) sub (base ? ${key}) (base.${key} or null)
      ) tree;
      gone = filter (key: tree.${key} == { remove = true; }) keys;
    in
    if isVerbs tree then
      verbsAt scope path tree present current
    else if !isAttrs tree then
      throw "${here}: expected a verb (${words verbs}), got a ${typeOf tree}"
    else if tree ? edit then
      throw "${here}.edit: edit goes on the package, not a field"
    else if verbal != [ ] then
      throw "${here}: mixes verbs (${words verbal}) with fields (${
        words (filter (key: !elem key verbal) keys)
      })"
    else
      # a removed child is still forced, so removing a missing field fails
      builtins.foldl' (result: key: builtins.seq children.${key} result) (removeAttrs (
        base // children
      ) gone) gone;

  verbsAt =
    scope: path: node: present: current:
    let
      fail = message: throw "${at scope path}: ${message}";
      packages = path == [ "dependencies" ] || path == [ "buildDependencies" ];
      read =
        verb: kind:
        let
          given = node.${verb};
        in
        if kind == "list" && isList given then
          if packages then
            map (
              item: if isString item then scope.set.${item} or (fail "no package named ${item}") else item
            ) given
          else
            given
        else if kind == "set" && isAttrs given then
          given
        else
          fail "${verb} takes a ${kind}, got a ${typeOf given}";

      whole = node ? remove && node.remove == true;
      some = node ? remove && !whole;
      grows = node ? prepend || node ? append;
      drop =
        if some then
          map (item: if packages && !isString item then nameOf item else item) (read "remove" "list")
        else
          [ ];
      keep = item: !elem (if packages then nameOf item else item) drop;

      start =
        if node ? set then
          if packages then read "set" "list" else node.set
        else if present && !whole then
          current
        else if node ? merge then
          { }
        else if grows then
          [ ]
        else if node ? remove then
          if present then null else fail "remove: no such field"
        else
          fail "missing, and only .set adds a field";
      dropped =
        if !some then
          start
        else if isList start then
          filter keep start
        else if isAttrs start then
          removeAttrs start drop
        else
          fail "remove from a ${typeOf start}, expected a list or set";
      merged =
        if !(node ? merge) then
          dropped
        else if !isAttrs dropped then
          fail "merge into a ${typeOf dropped}, expected a set"
        else
          dropped // removeAttrs (read "merge" "set") drop;
    in
    if !grows then
      merged
    else if !isList merged then
      fail "append to a ${typeOf merged}, expected a list"
    else
      filter keep (
        (if node ? prepend then read "prepend" "list" else [ ])
        ++ merged
        ++ (if node ? append then read "append" "list" else [ ])
      );

  applyOne =
    set: name: node: spec:
    let
      change = node.edit or (same: same);
    in
    if !isAttrs node then
      throw "overrides: ${name} must be an attribute set of fields, not a ${typeOf node}"
    else if !isFunction change then
      throw "overrides: ${name}.edit must map a spec to a spec, not be a ${typeOf change}"
    else
      change (descend { inherit name set; } [ ] (removeAttrs node [ "edit" ]) true spec);
in
{
  merge = trees: if isList trees then join trees else trees;

  inherit applyOne;

  # `features` is read before the spec exists, by nix/features.nix
  apply =
    set: tree: name: spec:
    let
      node = tree.${name};
    in
    if !(tree ? ${name}) then
      spec
    else
      applyOne set name (if isAttrs node then removeAttrs node [ "features" ] else node) spec;

  unknown = set: tree: filter (name: !(set ? ${name})) (attrNames tree);
}
