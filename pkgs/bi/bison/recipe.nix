{
  platform,
  pkgs,
  tools,
  when,
  ...
}:
[
  {
    autotools.flags = [ "M4=${pkgs.m4}/bin/m4" ];
    check.needs = [ tools.perl ];
  }
  (when (!platform.cross) { tools = [ pkgs.m4 ]; })
  (when platform.cross { tools = [ "m4" ]; })
]
