{
  platform,
  pkgs,
  when,
  ...
}:
[
  {
    autotools.flags = [
      "ac_cv_path_M4=${pkgs.m4}/bin/m4"
      "--disable-bootstrap"
    ];
  }
  (when (!platform.cross) { tools = [ pkgs.m4 ]; })
  (when platform.cross { tools = [ "m4" ]; })
]
