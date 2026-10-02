{
  platform,
  when,
  ...
}:
[
  (when platform.cross {
    autotools.flags = [ "--with-cross-build=/build/native-icu" ];
    before."autotools.configure" = [ "icu.cross-buildroot" ];
    check = false;
    tools = [ "icu" ];
  })
  (when (platform.os == "macos") { autotools.flags = [ "--enable-rpath" ]; })
]
