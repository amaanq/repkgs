{
  platform,
  tools,
  when,
  ...
}:
[
  { cmake.tool = tools.cmake-bootstrap; }
  (when (platform.libc == "msvc") { cmake.defs.BUILD_STATIC_LIBS = false; })
]
