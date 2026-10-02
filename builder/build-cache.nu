# Results worth keeping between builds, in jigd: configure caches (autoconf's config.cache, cmake's
# INTERNAL entries) and the cache directories of zig, hadrian, mix and rebar3. An entry's key is
# its inputs (scripts, flags, dependencies), not the derivation, so a recipe edit that leaves those
# alone still finds it.

use core.nu *

# `files`: the scripts whose results are kept. `args`: their command line, when the results depend
# on it (autoconf rejects a config.cache made under other CFLAGS=..). Always in the key: the store
# paths the build sees (toolchain and dependencies, which results mention by path), the cc flags
# jig injects, and $out unless --no-out (configure records the prefix)
export def key [kind: string, files: list<path>, args: list<string> = [], --no-out]: nothing -> string {
  let c = (ctx)
  let inputs = {
    files: ($files | sort | each { open --raw $in | hash sha256 })
    args: $args
    roots: $c.roots
    cc: ($env.PKGS_CC | from json | values)
    out: (if $no_out { null } else { $c.out })
  }
  $"build1/($kind)/($inputs | to json -r | hash sha256)"
}

# one file back from jigd. false: nothing there, or no daemon, which builds work without
export def restore [key: string, file: path]: nothing -> bool {
  (ctx).cache and (^jig cache get $key $file | complete).exit_code == 0
}

# best effort, a daemon that went away is no error
export def store [key: string, file: path]: nothing -> nothing {
  if not ((ctx).cache and ($file | path exists)) { return }
  if (^jig cache put $key $file | complete).exit_code != 0 { note build-cache $"put ($key) failed" }
}

# a directory tree, its file contents deduplicated in jigd with one listing per key
export def restore-dir [key: string, dir: path]: nothing -> bool {
  (ctx).cache and (^jig cache get-dir $key $dir | complete).exit_code == 0
}

export def store-dir [key: string, dir: path]: nothing -> nothing {
  if (ctx).cache and ($dir | path exists) { ^jig cache put-dir $key $dir }
}
