{
  platform,
  when,
  ...
}:
[
  (when (!platform.cross) {
    tools = [
      "rust-bootstrap"
      "cpython"
      "cmake"
      "ninja"
      "pkgconf"
    ];
  })
  (when platform.cross {
    tools = [
      "llvm22"
      "rust-bootstrap"
      "cpython"
      "cmake"
      "ninja"
      "pkgconf"
    ];
  })
]
