let
  data = builtins.fromJSON (builtins.readFile ./schema.json);
  fields = builtins.mapAttrs (
    _: rows:
    builtins.listToAttrs (
      map (row: {
        inherit (row) name;
        value = row;
      }) rows
    )
  ) data.systems;
  nixTypes =
    descriptor:
    if descriptor.kind == "union" then
      builtins.concatMap nixTypes descriptor.variants
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
  matches =
    descriptor: value:
    if descriptor.kind == "union" then
      builtins.any (variant: matches variant value) descriptor.variants
    else if descriptor.kind == "package" then
      builtins.isAttrs value && value ? outPath
    else if descriptor.kind == "enum" then
      builtins.elem value descriptor.values
    else
      builtins.elem (builtins.typeOf value) (nixTypes descriptor);
  describe =
    descriptor:
    if descriptor.kind == "union" then
      builtins.concatStringsSep " or " (map describe descriptor.variants)
    else if descriptor.kind == "package" then
      "package"
    else if descriptor.kind == "enum" then
      "one of ${builtins.concatStringsSep " " descriptor.values}"
    else
      builtins.head (nixTypes descriptor);
  checkValue =
    recursive: path: descriptor: value:
    let
      expected = describe descriptor;
      actual = if builtins.isString value then ''"${value}"'' else builtins.typeOf value;
    in
    if !(matches descriptor value) then
      [ "option ${path} is ${actual}, expected ${expected}" ]
    else if recursive && descriptor.kind == "union" then
      checkValue true path (builtins.head (
        builtins.filter (variant: matches variant value) descriptor.variants
      )) value
    else if recursive && descriptor.kind == "list" then
      builtins.concatLists (
        builtins.genList (
          index: checkValue true "${path}[${toString index}]" descriptor.element (builtins.elemAt value index)
        ) (builtins.length value)
      )
    else if recursive && descriptor.kind == "map" then
      builtins.concatMap (name: checkValue true "${path}.${name}" descriptor.value value.${name}) (
        builtins.attrNames value
      )
    else if recursive && descriptor.kind == "record" then
      builtins.concatMap (
        name:
        if descriptor.fields ? ${name} then
          checkValue true "${path}.${name}" descriptor.fields.${name} value.${name}
        else
          [
            "unknown option ${path}.${name} (have ${builtins.concatStringsSep " " (builtins.attrNames descriptor.fields)})"
          ]
      ) (builtins.attrNames value)
      ++ map (name: "option ${path}.${name} is missing") (
        builtins.attrNames (removeAttrs descriptor.fields (builtins.attrNames value))
      )
    else
      [ ];
in
{
  inherit (data) hardening;
  options = builtins.mapAttrs (
    _: declared:
    builtins.mapAttrs (_: field: {
      type = nixTypes field.type;
      inherit (field) default doc;
    }) declared
  ) fields;
  validate =
    {
      system,
      value,
      recursive ? true,
      extra ? { },
    }:
    let
      declared = fields.${system} // builtins.mapAttrs (_: type: { inherit type; }) extra;
    in
    if !builtins.isAttrs value then
      [ "option ${system} is ${builtins.typeOf value}, expected set" ]
    else
      builtins.concatMap (
        name:
        if !(declared ? ${name}) then
          [
            "unknown option ${system}.${name} (have ${builtins.concatStringsSep " " (builtins.attrNames declared)})"
          ]
        else
          checkValue recursive "${system}.${name}" declared.${name}.type value.${name}
      ) (builtins.attrNames value);
}
