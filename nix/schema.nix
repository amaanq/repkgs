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
  nixType =
    descriptor:
    {
      boolean = "bool";
      string = "string";
      list = "list";
    }
    .${descriptor.kind};
  checkValue =
    recursive: path: descriptor: value:
    let
      expected = nixType descriptor;
      actual = builtins.typeOf value;
    in
    if actual != expected then
      [ "option ${path} is a ${actual}, expected ${expected}" ]
    else if recursive && descriptor.kind == "list" then
      builtins.concatLists (
        builtins.genList (
          index: checkValue true "${path}[${toString index}]" descriptor.element (builtins.elemAt value index)
        ) (builtins.length value)
      )
    else
      [ ];
in
{
  inherit (data) hardening;
  options = builtins.mapAttrs (
    _: declared:
    builtins.mapAttrs (_: field: {
      type = [ (nixType field.type) ];
      inherit (field) default doc;
    }) declared
  ) fields;
  validate =
    {
      system,
      value,
      recursive ? true,
    }:
    let
      declared = fields.${system};
    in
    if !builtins.isAttrs value then
      [ "option ${system} is a ${builtins.typeOf value}, expected set" ]
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
