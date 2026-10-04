# spieler

The updater for this tree and any other that keeps its pins in `act.lock`. It lives in the mummer
workspace (`pkgs/mu/mummer/src/spieler`) and is on PATH in the development shell and through
`motley update`.

## Principles

- **Machines write data, never Nix.** The tool owns act.lock, a TOML file, and Nix only reads
  that file. Rewriting Nix source with regexes is what forces skip lists elsewhere.
- **The source names its own feed.** A package's purl chooses where releases are listed and how
  versions compare, and configuration is for exceptions.
- **Listing releases and comparing versions are separate concerns**, one a datasource and the
  other a versioning scheme.
- **Lock files need no handling.** Dynamic derivations fetch what a lock names with the lock's own
  hashes, so there are no vendor hashes to guess or rebuild for. Only ecosystems whose locks carry
  no file hashes get a table here.
- **Some packages need code**, for a second pin read out of the new source or files generated from
  it. Hooks are part of the pipeline, not a way around it.
- **Polling is the expensive part at scale**, so it is one cached pass that never runs a package's
  own code.

## `act.toml` and `act.lock`

People write `track` and `fetch` in a package's act.toml (docs/act.md), spieler writes its
act.lock. Nix reads both with `fromTOML`, and the rest of act.toml, the build, is the package set's
business and ignored here.

```toml
# act.toml
tag   = "v{version}"
track = "github:sharkdp/fd"        # a purl without pkg:, which picks the datasource and versioning
fetch = { archive = true }         # or { asset = "…" }, { url = "…" }, [fetch.<name>] tables
# unpack = false                   keep the file whole, the hash is then its flat sha256
# frozen = "distro build"          on a source, a url that ignores the version, which rehash leaves alone

[track]                            # instead of the plain string, when tracking needs options
# from = "github:sharkdp/fd"
# versioning = "semver" | "pep440" | "calver" | "loose"
# allow = ">=10,<11"               versions outside are never candidates
# prerelease = false
# every = "30d"                    a release must be this old
# base = "2.35.2"                  the release a ?branch= tip counts from, else the newest tag
# group = "llvm"
# cpe = "cpe:2.3:a:haxx:curl"      for NVD where OSV does not know the project
# frozen = "last release 2009"     instead of `from`, for a dead upstream nobody polls
# relocks = ["hackage"]            applying this re-solves that ecosystem's whole lock table
# watch = { url = "…/LATEST", regex = "([0-9.]+)" }   or { purl = "pkg:npm/x" }
# locks = { go = "." }             where in the source go.sum lives
```

```toml
# act.lock
version = "10.5.0"
date    = "2025-05-18"             # the release date, for `every`
sys     = [ "jemalloc" ]           # libraries of the set the source's lock files may link
hash    = "sha256-…"               # the NAR hash of the unpacked tree, top directory stripped

[vars]
# file = "25.0.4.1_1"              anything else a resolve hook returned, a {file} placeholder in urls
```

URLs name the canonical host, and nix/mirrors.nix adds fallbacks by prefix. `{version}`, `{tag}`,
`{version_}`, `{major}`, `{minor}` and every key of act.lock fill url placeholders. The lock holds
`tag` only where act.toml's template does not spell it. Keeping what spieler writes in its own file
means a version bump never touches the act a person wrote, and one pair per package makes a whole
tree one glob, usable without Nix too.

## Pipeline

```
discover → resolve → decide → apply → verify
```

**discover** globs `act.toml` and validates it with its act.lock. An unknown key is an error that
lists the known ones, and an act with no track, source, pin or locks describes a build only and is
skipped.

**resolve** alone talks to upstreams and only evaluates watches. Each datasource lists releases
for a purl (github, gitlab, pypi, crates, npm, hackage, gnu, `generic` listings with `?url=`, and
two vendor feeds, `visualstudio` for the Visual Studio release manifest and `applesdk` for the macOS
software update catalog). HTTP goes through a cache under `$XDG_CACHE_HOME/spieler` keeping etag,
last-modified and body per URL, so a run over an unchanged world is conditional requests answered
with 304.

A github or gitlab purl with `?branch=<name>` follows that branch instead of releases. The one
candidate is its newest commit, versioned `<base>-unstable-<commit day>` the way nixpkgs names
snapshots, with the commit in act.lock's `rev` for a `{rev}` url. The base is `[track] base` or the
newest stable tag among the first hundred the API lists. A tip that moved within the same day keeps
its version, so decide also proposes an update whenever the rev differs from the pinned one.

**decide** is a pure function from the manifest, the candidates and the flags to a plan entry, the
new version with its sources, pins and lock work, or a reason to skip. `check --json` prints the
plan, and `apply --plan` takes one back, edited or not.

**apply** substitutes the new pin into every URL, prefetches, writes act.lock, checks the package's
patches still apply to the new source (a patch the source already contains, or one that no longer
applies, puts act.lock back and names the patch), and runs the lock stage for packages with `[track]
locks`. That stage fetches the pinned source through the tree and records what the ecosystem's own
lock cannot give Nix in `$SPIELER_LOCKS/<eco>.toml` (`<root>/locks/` by default, and another tree
passes its own table to the fetcher):

- **go**: go.sum's `h1:` is a directory hash, so the table maps `module@version` to the sha256 of
  the proxy's `.mod` and `.zip`.
- **hackage**: there is no upstream lock, so cabal solves the set. Entries are keyed
  `name-version`, keeping several releases where packages disagree, with the tarball hash, the
  Cabal revision and its hash.
- **luarocks**: a version and hash per rock, from luarocks' own solve.

A table holds one sorted line per entry and merges as text (`merge=union` in .gitattributes),
with `spieler normalize` as the nuff step restoring the order. Cargo and npm locks carry file
hashes and need no table. `spieler lock` runs the stage alone, and an entry already matching
the pin is skipped, so an interrupted run picks up where it stopped.

**verify** builds through the tree (`nix-build -A <name>` here) and records what the build printed
as its version.

## Hooks

Most packages need nothing beyond `act.toml`. Where a stage needs code,
`pkgs/xx/<name>/update.nu` exports a replacement:

```nu
export def resolve [pkg: record]: nothing -> table    # [{version tag? date? prerelease …}], extra keys land in act.lock
export def files   [entry: record]: nothing -> record # {relative path: content}, written after the pin
export def verify  [entry: record]: nothing -> record # instead of nix-build
```

A hook runs in a fresh `nu` with spieler's directory on PATH, so `^spieler versions <purl>` and
`^spieler get <url> --max-age 1day` serve it through the same cache. It gets its record as JSON in
`$env.SPIELER_ARG`, answers in JSON on stdout and shows stderr. Hooks write no files themselves and
run only after a watch fired, and any other export is an error. `spieler list` shows which stages a
package replaces.

The tree's hooks show the range. llvm's `files` regenerates the per-cpu compiler-rt source lists
from the new tarball. rust-bootstrap's `resolve` follows the compiler rust's `src/stage0` names, and
llvm21's the LLVM major zig's `Findllvm.cmake` expects. jdk-bootstrap's adds the spellings of the
tag that Temurin's file names and URLs use.

## Commands

```
spieler list [names]                     parse and validate every act.toml
spieler check [names] [--json]           resolve and decide, exit 10 when updates are pending
spieler apply [names] [--plan f]         write act.lock, hook files and locks for pending updates
              [--commit]                 commits each update alone as `name: 1.2 -> 1.3`, with jj or git
spieler verify [names]                   build at the current pin
spieler rehash [names]                   prefetch again at the current pin, after a url edit
spieler lock [names] [--prune]           resolve the `[track] locks` of packages into locks/<eco>.toml
spieler patch [names] [--update]         download url patches beside the act and check they apply
spieler init <dir> <purl> <url-template> a new act.toml pinned to the newest release
spieler normalize                        rewrite locks/<eco>.toml in normal form
spieler versions <purl>                  a datasource's candidates as JSON, for hooks
spieler get <url> [--max-age d]          a cached GET as JSON, for hooks
```

spieler shares mummer's unpacking, so a prefetched hash matches what the set fetches, and the lock
side of builder/sys-libs.json with the builder. Not built yet are a dashboard of pending and held
updates, libyear and advisory reports (OSV by purl, NVD where `cpe` is set), and grouped updates.
