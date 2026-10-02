{
  platform,
  pkgs,
  when,
  ...
}:
[
  { make.configureFlags = [ "--with-lua=${pkgs.lua}" ]; }
  (when (!platform.cross) { tools = [ pkgs.unzip ]; })
  (when platform.cross { tools = [ "unzip" ]; })
]
