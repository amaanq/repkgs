{
  platform,
  when,
  ...
}:
[
  (when platform.cross { tools = [ "jdk" ]; })
]
