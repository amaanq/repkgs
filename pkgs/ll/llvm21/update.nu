# zig pins one LLVM major, named in its cmake/Findllvm.cmake as "expected LLVM <N>.x"
export def resolve [pkg: record]: nothing -> table {
  let zig   = (open ($pkg.dir | path dirname -n 2 | path join zi zig package.toml)).pin.version
  let url   = $"https://codeberg.org/ziglang/zig/raw/tag/($zig)/cmake/Findllvm.cmake"
  let reply = (^uptrack get $url --max-age 1day | from json)
  if $reply.status != 200 { error make {msg: $"GET ($url) returned ($reply.status)"} }
  let major = ($reply.body | parse -r 'expected LLVM (\d+)\.x' | get 0.capture0)
  ^uptrack versions $pkg.upstream.purl | from json | where ($it.version | split row "." | first) == $major
}
