{
  platform,
  when,
  ...
}:
[
  (when platform.cross {
    autotools.flags = [ "--with-cross-build=/build/native-icu" ];
    steps.before."autotools.configure" = [ "icu.cross-buildroot" ];
    test = false;
    tools = [ "icu" ];
  })
  (when (platform.os == "macos") { autotools.flags = [ "--enable-rpath" ]; })
]
