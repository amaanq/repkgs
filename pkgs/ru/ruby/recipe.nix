{
  platform,
  when,
  ...
}:
[
  (when platform.cross {
    autotools.flags = [ "--with-baseruby=ruby" ];
    before."autotools.install" = [ "ruby.restore-shebangs" ];
    tools = [ "ruby" ];
  })
]
