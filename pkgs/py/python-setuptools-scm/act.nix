{
  platform,
  pkgs,
  when,
  ...
}:
[
  (when (!platform.cross) { tools = [ pkgs.python-setuptools ]; })
  (when platform.cross { tools = [ "python-setuptools" ]; })
]
