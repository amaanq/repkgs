# A package's `features`: knobs its package.nix declares and reads back as values.
#
#   features = {
#     tls = { values = [ "openssl" "gnutls" "none" ]; default = "openssl"; doc = "TLS backend"; };
#     http3 = { default = false; };
#     codecs = { default = [ "opus" ]; doc = "…"; };
#   };
#   dependencies = on (features.tls != "none") [ pkgs.${features.tls} ];
#
# The type is the default's (bool, string, list, int), `values` limits a string or a list's
# elements to a set. Resolved once per package as default <- set-wide `features` argument (only
# names the package declares) <- `overrides.<pkg>.features`. A name the package does not declare
# or a value of another type is an error. The result is in the derivation (drv hash) and on it as
# `.features`.
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
  accepted =
    package: declared:
    mapAttrs (
      feature: value:
      let
        declaration = declared.${feature};
        chosen = if isList value then value else [ value ];
      in
      if typeOf value != typeOf declaration.default then
        throw "${package}: feature ${feature} wants a ${typeOf declaration.default}, got a ${typeOf value}"
      else if declaration ? values && !all (entry: elem entry declaration.values) chosen then
        throw "${package}: feature ${feature} = ${toString value}, not among ${toString declaration.values}"
      else
        value
    );
in
{
  # declared: the package's `features` field ({} when absent). setWide: the set's. local:
  # overrides.<pkg>.features
  resolve =
    package: declared: setWide: local:
    let
      undeclared = attrNames (removeAttrs local (attrNames declared));
    in
    if declared == { } && local == { } then
      { }
    else if undeclared != [ ] then
      throw "${package}: unknown features ${toString undeclared} (has: ${toString (attrNames declared)})"
    else
      mapAttrs (_: declaration: declaration.default) declared
      // accepted package declared (intersectAttrs declared setWide)
      // accepted package declared local;
}
