{
  platform,
  tools,
  when,
  ...
}:
[
  { cmake.tool = tools.cmake-bootstrap; }
  (when (platform.libc == "msvc") {
    cmake.defs = {
      CMAKE_USE_SYSTEM_LIBRARY_BZIP2 = false;
      CMAKE_USE_SYSTEM_LIBRARY_CURL = false;
      CMAKE_USE_SYSTEM_LIBRARY_FORM = false;
      CMAKE_USE_SYSTEM_LIBRARY_LIBARCHIVE = false;
      CMAKE_USE_SYSTEM_LIBRARY_LIBLZMA = false;
      CMAKE_USE_SYSTEM_LIBRARY_LIBRHASH = false;
      CMAKE_USE_SYSTEM_LIBRARY_ZSTD = false;
    };
    deps = [
      "cppdap"
      "expat"
      "jsoncpp"
      "libuv"
      "nghttp2"
      "zlib"
    ];
  })
  (when (platform.libc != "msvc") {
    deps = [
      "bzip2"
      "cppdap"
      "curl"
      "expat"
      "jsoncpp"
      "libarchive"
      "libuv"
      "ncurses"
      "nghttp2"
      "rhash"
      "xz"
      "zlib"
      "zstd"
    ];
  })
]
