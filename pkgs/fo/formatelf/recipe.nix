{
  platform,
  tools,
  when,
  ...
}:
[
  (when (!platform.cross) { cargo.tool = tools.rust-bootstrap; })
  (when (platform.os == "linux") { cargo.tool = tools.rust-bootstrap; })
]
