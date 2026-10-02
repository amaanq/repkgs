# Choices a package offers, declared as its `features` and read back as values:
#
#   features = {
#     tls = { values = [ "openssl" "gnutls" "none" ]; default = "openssl"; doc = "TLS backend"; };
#     http3 = { default = false; };
#   };
#
# A feature has its default's type, and `values` bounds a string or each item of a list. The set's
# `features` argument fills the names a package declares, then `overrides.<pkg>.features` wins.
# The result goes into the derivation and onto it as `.features`.
let
  inherit (builtins)
    all
    attrNames
    elem
    intersectAttrs
    isList
    mapAttrs
    typeOf
    ;
  checked =
    package: declared:
    mapAttrs (
      feature: value:
      let
        spec = declared.${feature};
        wanted = typeOf spec.default;
      in
      if typeOf value != wanted then
        throw "${package}: feature ${feature} wants a ${wanted}, got a ${typeOf value}"
      else if
        spec ? values && !all (item: elem item spec.values) (if isList value then value else [ value ])
      then
        throw "${package}: feature ${feature} = ${toString value}, not among ${toString spec.values}"
      else
        value
    );
in
{
  resolve =
    package: declared: everywhere: here:
    let
      strays = attrNames (removeAttrs here (attrNames declared));
      defaults = mapAttrs (_: spec: spec.default) declared;
    in
    if declared == { } && here == { } then
      { }
    else if strays != [ ] then
      throw "${package}: unknown features ${toString strays} (has: ${toString (attrNames declared)})"
    else
      defaults
      // checked package declared (intersectAttrs declared everywhere)
      // checked package declared here;
}
