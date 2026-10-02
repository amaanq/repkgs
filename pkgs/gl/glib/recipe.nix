{
  platform,
  pkgs,
  when,
  ...
}:
[
  (when (!platform.cross) {
    tools = [
      pkgs.cpython
      "python-packaging"
    ];
  })
  (when platform.cross {
    tools = [
      "cpython"
      "python-packaging"
    ];
  })
]
