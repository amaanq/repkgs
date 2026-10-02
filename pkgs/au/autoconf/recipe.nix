{
  platform,
  pkgs,
  when,
  ...
}:
[
  (when (!platform.cross) {
    tools = [
      pkgs.m4
      pkgs.perl
    ];
  })
  (when platform.cross {
    tools = [
      "m4"
      "perl"
    ];
  })
]
