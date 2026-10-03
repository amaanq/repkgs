{
  platform,
  tools,
  when,
  ...
}:
[
  { cmake.tool = tools.cmake-bootstrap; }
  (when (platform.libc == "msvc") {
    remove = [
      "lib/libuv.lib"
      "lib/pkgconfig/libuv-static.pc"
    ];
  })
]
