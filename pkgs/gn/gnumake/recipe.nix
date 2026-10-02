{
  platform,
  tools,
  when,
  ...
}:
[
  { check.needs = [ tools.perl ]; }
  (when (platform.os == "windows") { cc.hardening.format = false; })
]
