# Passed to package.nix files as `lib`, and `on` on its own
{
  # the value when the condition holds, else the empty list or set. A string has no empty form here
  on =
    condition: value:
    if condition then
      value
    else if builtins.isAttrs value then
      { }
    else
      [ ];
  join = builtins.concatStringsSep;
}
