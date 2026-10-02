# Edits to package specs from outside the tree, passed as `overrides` (docs/design.md, Overrides):
#
#   import ./. { overrides = { curl.autotools.flags.prepend = [ "--disable-ldap" ]; }; }
#
# The tree is <package>.<field>….<verb>. On a field
#   set       the value, created when missing
#   append    list items at the end, prepend at the front
#   merge     attributes, with //
#   remove    list items or attribute names, or true for the field itself
# and on a package
#   edit      spec -> spec, run after everything else
#   features  { <feature> = value; }, read by nix/features.nix
# A node starts from set or the field, removes, merges, then prepends and appends. merge, append
# and prepend create a missing field or refill one remove = true took, and an item remove also
# drops what they add. Under dependencies and buildDependencies a string names a package of the
# final set and remove compares package names. A list of trees joins into one first, where set
# and edit keep the last, list verbs concatenate and remove = true beats item removes. Any path or
# argument that does not fit is an error.
let
  inherit (builtins)
    all
    attrNames
    concatMap
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
  verbal = node: isAttrs node && node != { } && all isVerb (attrNames node);
  # a key spelled like a verb whose value is itself a node is a field (phases.remove.set)
  usedAsVerb = tree: key: isVerb key && !verbal tree.${key};
  last = values: elemAt values (length values - 1);
  same = spec: spec;

  join = zipAttrsWith (
    key: values:
    if isVerb key && all verbal values then
      join values
    else if key == "set" || key == "edit" then
      last values
    else if key == "features" || key == "merge" then
      foldl' (left: right: left // right) { } values
    else if key == "remove" && elem true values then
      true
    else if key == "remove" || key == "append" || key == "prepend" then
      concatMap (
        value: if isList value then value else throw "overrides: ${key} takes a list, got a ${typeOf value}"
      ) values
    else if all isAttrs values then
      join values
    else
      throw "overrides: conflicting values at ${key}"
  );

  packageName = dependency: dependency.pname or dependency.name or (toString dependency);

  # one verb node against the field's value, as a run of stages over it
  verbsOn =
    context: path: node: present: current:
    let
      fail = message: throw "overrides: ${context.package}.${concatStringsSep "." path}: ${message}";
      packageList =
        length path == 1
        && elem (head path) [
          "dependencies"
          "buildDependencies"
        ];
      given =
        verb: kind:
        let
          value = node.${verb};
          named =
            item:
            if packageList && isString item then
              context.pkgs.${item} or (fail "no package named ${item}")
            else
              item;
        in
        if kind == "list" && isList value then
          map named value
        else if kind == "set" && isAttrs value then
          value
        else
          fail "${verb} takes a ${kind}, got a ${typeOf value}";
      whole = node.remove or null == true;
      some = node ? remove && !whole;
      grows = node ? prepend || node ? append;
      removed =
        if some then
          map (item: if packageList && !isString item then packageName item else item) (given "remove" "list")
        else
          [ ];
      kept = item: !elem (if packageList then packageName item else item) removed;

      initial =
        if node ? set then
          (if packageList then given "set" "list" else node.set)
        else if present && !whole then
          current
        else if node ? merge then
          { }
        else if grows then
          [ ]
        else if node ? remove then
          (if present then null else fail "remove: no such field")
        else
          fail "missing, and only .set adds a field";
      drop =
        value:
        if !some then
          value
        else if isList value then
          filter kept value
        else if isAttrs value then
          removeAttrs value removed
        else
          fail "remove from a ${typeOf value}, expected a list or set";
      combine =
        value:
        if !(node ? merge) then
          value
        else if isAttrs value then
          value // removeAttrs (given "merge" "set") removed
        else
          fail "merge into a ${typeOf value}, expected a set";
      extend =
        value:
        if !grows then
          value
        else if !isList value then
          fail "append to a ${typeOf value}, expected a list"
        else
          filter kept (
            (if node ? prepend then given "prepend" "list" else [ ])
            ++ value
            ++ (if node ? append then given "append" "list" else [ ])
          );
    in
    foldl' (value: stage: stage value) initial [
      drop
      combine
      extend
    ];

  # the tree and the spec side by side, a field whose node is { remove = true; } leaves the result
  descend =
    context: path: tree: present: current:
    let
      at = "overrides: ${context.package}.${concatStringsSep "." path}";
      keys = attrNames tree;
      verbKeys = filter (usedAsVerb tree) keys;
      fieldKeys = filter (key: !usedAsVerb tree key) keys;
      base =
        if !present then
          { }
        else if isAttrs current then
          current
        else
          throw "${at}: is a ${typeOf current}, cannot descend";
      children = mapAttrs (
        key: subtree: descend context (path ++ [ key ]) subtree (base ? ${key}) (base.${key} or null)
      ) tree;
      vanished = filter (
        key: tree.${key} == { remove = true; } && builtins.seq children.${key} true
      ) keys;
    in
    if verbal tree then
      verbsOn context path tree present current
    else if !isAttrs tree then
      throw "${at}: expected a verb (${concatStringsSep " " verbs}), got a ${typeOf tree}"
    else if tree ? edit then
      throw "${at}.edit: edit goes on the package, not a field"
    else if verbKeys != [ ] then
      throw "${at}: mixes verbs (${toString verbKeys}) with fields (${toString fieldKeys})"
    else
      removeAttrs (base // children) vanished;

  applyOne =
    pkgs: name: node: spec:
    if !isAttrs node then
      throw "overrides: ${name} must be an attribute set of fields, not a ${typeOf node}"
    else if !isFunction (node.edit or same) then
      throw "overrides: ${name}.edit must map a spec to a spec, not be a ${typeOf node.edit}"
    else
      let
        fields = removeAttrs node [ "edit" ];
        place = {
          package = name;
          inherit pkgs;
        };
      in
      (node.edit or same) (descend place [ ] fields true spec);
in
{
  merge = trees: if isList trees then join trees else trees;
  inherit applyOne;
  # nix/set.nix resolved `<package>.features` before the spec existed
  apply =
    pkgs: tree: name: spec:
    if !(tree ? ${name}) then
      spec
    else if isAttrs tree.${name} then
      applyOne pkgs name (removeAttrs tree.${name} [ "features" ]) spec
    else
      applyOne pkgs name tree.${name} spec;
  unknown = pkgs: tree: filter (name: !(pkgs ? ${name})) (attrNames tree);
}
