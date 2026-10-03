# Handed to every recipe as `lib`, so it holds only what recipes use.
{
  on =
    cond: value:
    if cond then
      value
    else if builtins.isAttrs value then
      { }
    else
      [ ];
  join = builtins.concatStringsSep;
  check =
    context: problems:
    let
      all = builtins.concatLists problems;
    in
    all == [ ] || throw "${context}: ${builtins.concatStringsSep "; " all}";
}
