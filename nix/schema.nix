# Checks run at three depths. "outer" forces only the value, "keys" walks records and maps but
# leaves strings, paths and packages unforced, since forcing an interpolated dependency instantiates
# it and a support query must not, and "deep" runs where the value is read
let
  inherit (builtins)
    any
    attrNames
    concatMap
    concatStringsSep
    elem
    filter
    genList
    head
    isAttrs
    isString
    length
    listToAttrs
    mapAttrs
    removeAttrs
    typeOf
    ;
  data = builtins.fromJSON (builtins.readFile ./schema.json);
  byName =
    rows:
    listToAttrs (
      map (row: {
        inherit (row) name;
        value = row;
      }) rows
    );
  fields = mapAttrs (_: byName) data.systems;
  recipe = byName data.recipe;
  nixTypes =
    descriptor:
    if descriptor.kind == "union" then
      concatMap nixTypes descriptor.variants
    else
      [
        {
          boolean = "bool";
          string = "string";
          integer = "int";
          path = "path";
          package = "set";
          null = "null";
          list = "list";
          map = "set";
          enum = "string";
          record = "set";
        }
        .${descriptor.kind}
      ];
  stringy =
    descriptor:
    elem descriptor.kind [
      "string"
      "path"
      "package"
    ]
    || descriptor.kind == "union" && any stringy descriptor.variants;
  matches =
    descriptor: value:
    if descriptor.kind == "union" then
      any (variant: matches variant value) descriptor.variants
    else if descriptor.kind == "package" then
      isAttrs value && value ? outPath
    else if descriptor.kind == "enum" then
      elem value descriptor.values
    else
      elem (typeOf value) (nixTypes descriptor);
  describe =
    descriptor:
    if descriptor.kind == "union" then
      concatStringsSep " or " (map describe descriptor.variants)
    else if descriptor.kind == "package" then
      "package"
    else if descriptor.kind == "enum" then
      "one of ${concatStringsSep " " descriptor.values}"
    else
      head (nixTypes descriptor);
  unknown = path: names: "unknown ${path} (have ${concatStringsSep " " names})";
  check =
    mode: path: descriptor: value:
    let
      deeper = mode != "outer";
      shown = if isString value then ''"${value}"'' else typeOf value;
    in
    if mode == "keys" && stringy descriptor then
      [ ]
    else if !(matches descriptor value) then
      [ "${path} is ${shown}, expected ${describe descriptor}" ]
    else if deeper && descriptor.kind == "union" then
      check mode path (head (filter (variant: matches variant value) descriptor.variants)) value
    else if deeper && descriptor.kind == "list" then
      if mode == "keys" && stringy descriptor.element then
        [ ]
      else
        concatMap (
          index: check mode "${path}[${toString index}]" descriptor.element (builtins.elemAt value index)
        ) (genList (index: index) (length value))
    else if deeper && descriptor.kind == "map" then
      concatMap (
        name:
        if descriptor ? key && !(matches descriptor.key name) then
          [ "${path}.${name} is not ${describe descriptor.key}" ]
        else
          check mode "${path}.${name}" descriptor.value value.${name}
      ) (attrNames value)
    else if deeper && descriptor.kind == "record" then
      record mode "${path}." descriptor.fields (descriptor.optional or [ ]) value
    else
      [ ];
  record =
    mode: prefix: declared: optional: value:
    concatMap (
      name:
      if declared ? ${name} then
        check mode "${prefix}${name}" declared.${name} value.${name}
      else
        [ (unknown "${prefix}${name}" (attrNames declared)) ]
    ) (attrNames value)
    ++ map (name: "${prefix}${name} is missing") (
      filter (name: !(elem name optional)) (attrNames (removeAttrs declared (attrNames value)))
    );
in
{
  inherit (data) hardening;
  recipeFields = attrNames recipe;
  options = mapAttrs (
    _: declared:
    mapAttrs (_: field: {
      type = nixTypes field.type;
      inherit (field) default doc;
    }) declared
  ) fields;
  # a recipe's record leaves out what `lock.deps` fills in, so no option is required there
  validate =
    {
      system,
      value,
      mode ? "deep",
      extra ? { },
    }:
    let
      declared = mapAttrs (_: field: field.type) fields.${system} // extra;
    in
    if !isAttrs value then
      [ "${system} is ${typeOf value}, expected set" ]
    else
      record mode "${system}." declared (attrNames declared) value;
  validateRecipe =
    { value, mode }:
    record mode "" (mapAttrs (_: field: field.type) recipe) (filter (
      name: !(recipe.${name}.required or false)
    ) (attrNames recipe)) value;
  checkField =
    mode: name: value:
    check mode name recipe.${name}.type value;
}
