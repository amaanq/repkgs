{
  platform,
  when,
  ...
}:
[
  (when platform.cross {
    autotools.flags = [ "--with-baseruby=ruby" ];
    steps.before."autotools.install" = [ "ruby.restore-shebangs" ];
    tools = [ "ruby" ];
  })
]
