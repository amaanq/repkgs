# A package's feature values, its defaults under the set-wide values it declares under its own
# override entry. Values are checked lazily, so one shadowed by the package's own never fails.
{
  resolve =
    name: declared: setWide: own:
    let
      names = builtins.attrNames declared;
      unknown = builtins.filter (n: !(declared ? ${n})) (builtins.attrNames own);
      words = builtins.concatStringsSep " ";

      checked =
        feature: value:
        let
          decl = declared.${feature};
          want = builtins.typeOf decl.default;
          got = builtins.typeOf value;
          items = if builtins.isList value then value else [ value ];
        in
        if got != want then
          throw "${name}: feature ${feature} wants a ${want}, got a ${got}"
        else if decl ? values && !builtins.all (item: builtins.elem item decl.values) items then
          throw "${name}: feature ${feature} = ${toString value}, not among ${toString decl.values}"
        else
          value;
      given = values: builtins.mapAttrs checked values;
    in
    if declared == { } && own == { } then
      { }
    else if unknown != [ ] then
      throw "${name}: unknown features ${words unknown} (has: ${words names})"
    else
      builtins.mapAttrs (_: decl: decl.default) declared
      // given (builtins.intersectAttrs declared setWide)
      // given own;
}
