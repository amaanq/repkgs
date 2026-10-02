# fix-shebangs (core.nu) points interpreters at PATH, keeps their arguments, and --undo restores
# what it changed
use core.nu *
use checks.nu *

let dir = (mktemp -d)
$env.NIX_BUILD_TOP = (mktemp -d)
let scripts = {
  env: "#!/usr/bin/env sh\necho"
  args: "#! /usr/bin/env sh -e\necho"
  abs: "#!/usr/local/bin/nu --stdin\n1"
  sh: "#!/bin/sh\necho"
  body: "#!/bin/sh\n#!/bin/bash in the body"
  stub: "#!/bin/sh\nexec ruby -x \"$0\"\n#!/usr/bin/env nu\n1"
  missing: "#!/usr/bin/nonexistent\n"
}
for script in ($scripts | transpose name text) {
  $script.text | save -f $"($dir)/($script.name)"
  ^chmod +x $"($dir)/($script.name)"
}
let sh      = (which sh | get 0.path)
let nushell = (which nu | where type == external | get 0.path)
let now     = {|name| open --raw $"($dir)/($name)" }

fix-shebangs $dir
expect "env finds sh on PATH"                         ((do $now env) == $"#!($sh)\necho")
expect "no line is added to an escript-style header"  ((do $now abs | lines | length) == 2)
expect "interpreter arguments survive"                ((do $now args | lines | first) == $"#!($sh) -e")
expect "an absolute interpreter moves to PATH's"      ((do $now abs | lines | first) == $"#!($nushell) --stdin")
expect "/bin/sh is left alone"                        ((do $now sh) == $scripts.sh)
expect "absolute paths change on the first line only" ((do $now body) == $scripts.body)
expect "an env line counts anywhere"                  ((do $now stub | lines | get 2) == $"#!($nushell)")
expect "an interpreter missing from PATH stays"       ((do $now missing) == $scripts.missing)
^sh $"($dir)/args"

# a build system writes these after prepare (pyapp entry points), so undo must not touch them
$"#!($sh)\nimport sys" | save -f $"($dir)/generated"
fix-shebangs $dir --undo
for name in [env args abs stub] { expect $"undo restores ($name)" ((do $now $name) == ($scripts | get $name)) }
expect "undo leaves a later script alone" ((do $now generated) == $"#!($sh)\nimport sys")
pass
