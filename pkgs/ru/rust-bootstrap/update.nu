# x.py accepts only the stage0 compiler its src/stage0 names, so this follows the rust pin
export def resolve [pkg: record]: nothing -> table {
  let rust  = (open --raw ($pkg.dir | path dirname | path join rust act.lock) | from toml).version
  let url   = $"https://raw.githubusercontent.com/rust-lang/rust/($rust)/src/stage0"
  let reply = (^spieler get $url --max-age 1day | from json)
  if $reply.status != 200 { error make {msg: $"GET ($url) returned ($reply.status)"} }
  let compiler = ($reply.body | lines | parse "compiler_version={version}" | get 0.version)
  [{version: $compiler, date: null, prerelease: false}]
}
