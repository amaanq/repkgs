{
  platform,
  tools,
  when,
  ...
}:
[
  (when platform.cross { tools = [ "ncurses" ]; })
  (when (platform.os == "windows") {
    autotools.flags = [
      "--enable-term-driver"
      "--enable-sp-funcs"
      "--with-tic-path=${tools.ncurses}/bin/tic"
      "--with-infocmp-path=${tools.ncurses}/bin/infocmp"
    ];
  })
  (when (platform.os != "windows") { autotools.flags = [ "--with-termlib" ]; })
  (when (platform.cpu != "x86_64") {
    autotools.flags = [
      "--with-tic-path=${tools.ncurses}/bin/tic"
      "--with-infocmp-path=${tools.ncurses}/bin/infocmp"
    ];
  })
]
