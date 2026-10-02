{
  platform,
  pkgs,
  toolchain,
  when,
  ...
}:
[
  {
    autotools.flags = [
      "--enable-kernel-poll"
      "--with-ssl=${pkgs.openssl}"
      "--without-javac"
      "--without-odbc"
      "--without-wx"
      "--disable-parallel-configure"
    ];
  }
  (when platform.cross {
    autotools.flags = [ "erl_xcomp_sysroot=${toolchain.sysroot}" ];
    tools = [ "erlang" ];
  })
]
