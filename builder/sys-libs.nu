# Locked third-party packages that link a C library, per ecosystem in sys-libs.json: which
# package of ours provides it (`pkg`) and how the build is told to use it instead of a bundled
# copy (`env`, plus `tags` for go and `flags` for bundler as `bundle config build.<gem>`
# arguments, `{root}` stands for the library's store path). Keyed by the lock file's names,
# Cargo.lock packages, go.sum module paths (cgo), uv.lock names (built from sdist so they link
# ours instead of a wheel's copy), Gemfile.lock gems, and hackage packages from the solved plan.
#
# Decided when the package is pinned: uptrack looks the lock files of the tree it just hashed up
# here (`wanted`) and writes `sys = [..]` into [pin] of sources.toml, nix/package.nix makes those
# dependencies. At build time jig's build systems apply env, tags and flags for the libraries
# present and check the lock still agrees with sys.
#
# Explicit on purpose: linking a system library is a decision. Packages that only ever vendor
# (ring, aws-lc-sys, modernc.org/sqlite) or are plain OS bindings have no entry.
# The tables are builder/sys-libs.json (nix/python.nix reads the python one too): per ecosystem,
# locked name -> {pkg, env, …}. python entries may lack `pkg` and carry `tools` (build programs
# by package name) and `configSettings` for the sdist build
const TABLES_FILE = path self ./sys-libs.json
export def tables []: nothing -> record { open $TABLES_FILE }

# --- lock side: uptrack writes [pin] sys, the build checks it --------------------------------------

const LOCKS = {cargo: Cargo.lock, go: go.sum, python: uv.lock, gems: Gemfile.lock}

# package names in a lock file
def locked [ecosystem: string, f: path]: nothing -> list<string> {
  match $ecosystem {
    "cargo" | "python" => { open --raw $f | from toml | get -o package | default [] | get name }
    "go" => { open --raw $f | lines | where $it != "" | split column " " path | get path }
    "gems" => { open --raw $f | lines | parse -r '^    (?<name>[A-Za-z0-9_.-]+) \(' | get name }
  }
}

# our packages the locked `names` of one ecosystem can link, sorted
export def wanted-for [ecosystem: string, names: list<string>]: nothing -> list<string> {
  (tables) | get $ecosystem | transpose locked entry | where locked in $names | each { $in.entry.pkg? } | compact | uniq | sort
}

# our packages the lock files in `dir` can link, sorted: what [pin] sys should say
export def wanted [dir: path]: nothing -> list<string> {
  $LOCKS | items {|eco, file|
    let f = ($dir | path join $file)
    if ($f | path exists) { wanted-for $eco (locked $eco $f) } else { [] }
  } | flatten | uniq | sort
}
