{
  platform,
  toolchain,
  source,
  file,
  when,
  ...
}:
[
  (when platform.cross {
    env = {
      PERL_CROSS = source "cross";
      PERL_CROSS_PATCH = file "upstream-cross-darwin.patch";
      PERL_MM_SHEBANG = "relocatable";
      PERL_SYSROOT = toolchain.sysroot;
    };
  })
]
