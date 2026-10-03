{
  platform,
  pkgs,
  when,
  ...
}:
[
  (when (!platform.cross) { tools = [ pkgs.erlang ]; })
  (when platform.cross { tools = [ "erlang" ]; })
]
