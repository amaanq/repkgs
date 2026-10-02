{ platform, when, ... }:
[
  (when (platform.os == "windows") {
    autotools = {
      flags = [ "CFLAGS=-D__USE_MINGW_ALARM -D_POSIX" ];
      makeFlags = [ "SHOBJ_LDFLAGS=-shared -Wl,--export-all-symbols -Wl,--enable-auto-import" ];
    };
  })
]
