{
  platform,
  tools,
  when,
  ...
}:
[
  { test.deps = [ tools.perl ]; }
  (when (platform.os == "windows") { cc.hardening.format = false; })
]
