{
  platform,
  tools,
  when,
  ...
}:
[
  (when platform.cross { make.flags = [ "zic=${tools.tzdata}/bin/zic" ]; })
]
