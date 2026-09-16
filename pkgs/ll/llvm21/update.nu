# uptrack hook. The LLVM major is zig's choice: cmake/Findllvm.cmake of the pinned zig says
# "expected LLVM <N>.x", and only releases of that major are candidates.

export def resolve [pkg: record]: nothing -> table {
  let zig = (open ($pkg.dir | path dirname -n 2 | path join zi zig sources.toml)).pin.version
  let url = $"https://codeberg.org/ziglang/zig/raw/tag/($zig)/cmake/Findllvm.cmake"
  let r = (^uptrack get $url --max-age 1day | from json)
  if $r.status != 200 { error make {msg: $"GET ($url) → ($r.status)"} }
  let major = ($r.body | parse -r 'expected LLVM (\d+)\.x' | get 0.capture0)
  ^uptrack versions $pkg.upstream.purl | from json | where { ($in.version | split row "." | first) == $major }
}
