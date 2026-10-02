{
  platform,
  file,
  when,
  ...
}:
[
  (when (!platform.cross) { before."autotools.test" = [ "tre.latin1" ]; })
  (when (platform.os == "windows") {
    after."autotools.install" = [ "tre.posix-regex" ];
    env.SYSTRE = file "systre.c";
  })
]
