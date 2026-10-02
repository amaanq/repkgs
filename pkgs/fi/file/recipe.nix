{
  platform,
  when,
  ...
}:
[
  (when platform.cross {
    autotools.makeFlags = [ "FILE_COMPILE=file" ];
    tools = [ "file" ];
  })
  (when (platform.os == "windows") { needs = [ "tre" ]; })
]
