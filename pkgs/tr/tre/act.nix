{
  platform,
  file,
  when,
  ...
}:
[
  (when (!platform.cross) { steps.before."autotools.test" = [ "tre.latin1" ]; })
  (when (platform.os == "windows") {
    steps.after."autotools.install" = [ "tre.posix-regex" ];
    env.SYSTRE = file "systre.c";
  })
]
