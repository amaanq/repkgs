# Overrides: edit package specs from outside the tree (docs/design.md "Overrides"), and the same
# verbs in-tree as `variant pkgs.llvm { cmake.defs.merge = { … }; }` in a package.nix.
#
#   import ./. { overrides = { jq.autotools.flags.append = [ "--x" ]; }; }
#
# A tree of <package>.<field>….<verb>. Verbs:
#   set      replace the value (creates the field if missing)
#   append   add to the end of a list        prepend  add to the front
#   merge    // into an attrset               remove   drop list elements / attr names, or `true`: the field
#   edit     on a package only: a function spec -> spec, applied after the other verbs
#   features on a package only: { <feature> = value; }, no verbs (nix/features.nix)
# One node applies as set, remove, merge, prepend/append: `merge`/`append`/`prepend` create a
# missing field (or refill one `remove = true` dropped), and a list `remove` also filters what
# merge/append/prepend add. In dependencies/buildDependencies a string names a package of the
# final set, and remove matches by package name. `overrides` may be a list of trees: they are
# merged first (set/edit: last wins, list verbs concatenate, `remove = true` beats element
# removes), then applied once per package. Unknown package, remove of a missing field, a verb
# argument of the wrong type, or a verb that does not fit the value's type is an error.
let
  inherit (builtins)
    all
    attrNames
    concatLists
    concatStringsSep
    elem
    elemAt
    filter
    foldl'
    head
    isAttrs
    isFunction
    isList
    isString
    length
    mapAttrs
    partition
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
  isVerbNode = node: isAttrs node && node != { } && all (key: elem key verbs) (attrNames node);
  union = foldl' (left: right: left // right) { };
  identity = spec: spec;

  combine =
    trees:
    zipAttrsWith (
      key: values:
      # a field that happens to be called like a verb (phases.remove) holds verb nodes itself
      if elem key verbs && all isVerbNode values then
        combine values
      else if key == "set" || key == "edit" then
        elemAt values (length values - 1)
      else if key == "features" || key == "merge" then
        union values
      else if key == "remove" && elem true values then
        true
      else if
        elem key [
          "append"
          "prepend"
          "remove"
        ]
      then
        concatLists (
          map (
            value: if isList value then value else throw "overrides: ${key} takes a list, got a ${typeOf value}"
          ) values
        )
      else if all isAttrs values then
        combine values
      else
        throw "overrides: conflicting values at ${key}"
    ) trees;

  # dependency lists: strings resolve in pkgs, remove matches by package name
  nameOf = dependency: dependency.pname or dependency.name or (toString dependency);

  applyNode =
    context: path: node: present: current:
    let
      fail = message: throw "overrides: ${context.package}.${concatStringsSep "." path}: ${message}";
      dependencies =
        length path == 1
        && elem (head path) [
          "dependencies"
          "buildDependencies"
        ];
      resolve =
        value:
        if dependencies && isString value then
          context.pkgs.${value} or (fail "no package named ${value}")
        else
          value;
      argument =
        verb: kind:
        let
          value = node.${verb};
        in
        if kind == "list" && isList value then
          map resolve value
        else if kind == "set" && isAttrs value then
          value
        else
          fail "${verb} takes a ${kind}, got a ${typeOf value}";
      extends = node ? prepend || node ? append;
      dropsSome = node ? remove && node.remove != true;
      dropped =
        if dropsSome then
          map (entry: if dependencies && !isString entry then nameOf entry else entry) (
            argument "remove" "list"
          )
        else
          [ ];
      kept = entry: !(elem (if dependencies then nameOf entry else entry) dropped);
      # the value the other verbs edit: set, else the field, else what merge/append imply
      start =
        if node ? set then
          (if dependencies then argument "set" "list" else node.set)
        else if present && (node.remove or null) != true then
          current
        else if node ? merge then
          { }
        else if extends then
          [ ]
        else if node ? remove then
          (if present then null else fail "remove: no such field")
        else
          fail "no such field (use .set to create it)";
      trimmed =
        if !dropsSome then
          start
        else if isList start then
          filter kept start
        else if isAttrs start then
          removeAttrs start dropped
        else
          fail "remove from a ${typeOf start}, expected a list or set";
      merged =
        if !(node ? merge) then
          trimmed
        else if isAttrs trimmed then
          trimmed // removeAttrs (argument "merge" "set") dropped
        else
          fail "merge into a ${typeOf trimmed}, expected a set";
    in
    if !extends then
      merged
    else if !isList merged then
      fail "append to a ${typeOf merged}, expected a list"
    else
      filter kept (
        (if node ? prepend then argument "prepend" "list" else [ ])
        ++ merged
        ++ (if node ? append then argument "append" "list" else [ ])
      );

  # walk tree and spec together. Returns the edited value, or null with remove = true handled by the caller
  walk =
    context: path: tree: present: current:
    let
      here = "overrides: ${context.package}.${concatStringsSep "." path}";
      # a verb name whose value is not itself a node was meant as a verb (a field called `remove`
      # is still reachable: phases.remove.set = [ … ])
      split = partition (key: elem key verbs && !isVerbNode tree.${key}) (attrNames tree);
      base =
        if present && isAttrs current then
          current
        else if present then
          throw "${here}: is a ${typeOf current}, cannot descend"
        else
          { };
      edited = mapAttrs (
        key: subtree: walk context (path ++ [ key ]) subtree (base ? ${key}) (base.${key} or null)
      ) tree;
      gone = filter (key: tree.${key} == { remove = true; } && builtins.seq edited.${key} true) (
        attrNames tree
      );
    in
    if isVerbNode tree then
      applyNode context path tree present current
    else if !isAttrs tree then
      throw "${here}: expected a verb (${concatStringsSep " " verbs}), got a ${typeOf tree}"
    else if tree ? edit then
      throw "${here}.edit: edit goes on the package, not a field"
    else if split.right != [ ] then
      throw "${here}: mixes verbs (${toString split.right}) with fields (${toString split.wrong})"
    else
      removeAttrs (base // edited) gone;

  # one package's subtree applied to its spec (also what `variant` in package.nix files uses)
  applyOne =
    pkgs: name: node: spec:
    if !isAttrs node then
      throw "overrides: ${name}: expected { <field>… }, got a ${typeOf node}"
    else if !isFunction (node.edit or identity) then
      throw "overrides: ${name}.edit: expected a function spec -> spec, got a ${typeOf node.edit}"
    else
      (node.edit or identity) (
        walk {
          package = name;
          inherit pkgs;
        } [ ] (removeAttrs node [ "edit" ]) true spec
      );
in
{
  merge = trees: if isList trees then combine trees else trees;
  inherit applyOne;
  # from the user's tree, where `<pkg>.features` is plain values that nix/set.nix resolved
  # before the spec existed
  apply =
    pkgs: tree: name: spec:
    if !(tree ? ${name}) then
      spec
    else if !isAttrs tree.${name} then
      applyOne pkgs name tree.${name} spec
    else
      applyOne pkgs name (removeAttrs tree.${name} [ "features" ]) spec;
  # packages the tree names that the set lacks
  unknown = pkgs: tree: filter (name: !(pkgs ? ${name})) (attrNames tree);
}
