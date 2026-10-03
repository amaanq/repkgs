use core.nu *

# zig speaks no cache protocol, but its global cache records paths relative to its roots, so the
# whole directory moves between sandboxes
def cache-key []: nothing -> string {
  let given = (attrs)
  ^bigtop probe key $"zig-cache/($given.src | path basename)" ...$given.patches
}

export def restore []: nothing -> nothing {
  let restored = (^bigtop probe restore-dir (cache-key) $env.ZIG_GLOBAL_CACHE_DIR | complete).exit_code == 0
  note zig-cache (if $restored { "restored" } else { "cold" })
}

export def store []: nothing -> nothing { ^bigtop probe store-dir (cache-key) $env.ZIG_GLOBAL_CACHE_DIR }
