{
  platform,
  tools,
  when,
  ...
}:
[
  { cmake.tool = tools.cmake-bootstrap; }
  (when platform.cross {
    cmake.skipTests = [
      "add_subdirectory"
      "find_package"
    ];
  })
]
