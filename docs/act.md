# act.toml and act.lock

Every package directory holds an `act.toml` that a person writes and an `act.lock` that spieler
writes. An `act.nix` is there only when something needs Nix (a fetcher, a local tree, the
toolchain). The directory name is the package name unless `name` says otherwise.

## act.toml

```toml
tag   = "v{version}"
track = "github:madler/zlib"
fetch = { asset = "zlib-{version}.tar.gz" }
with  = [ "cmake" ]

[test]
parallel = false

[cmake]
tool      = "cmake-bootstrap"
skipTests = [ "coverage" ]
defs      = { ZLIB_BUILD_EXAMPLES = false }

[on.cross.cmake]
skipTests = [ "add_subdirectory", "find_package" ]
```

### Identity

`tag` is how upstream spells a release, `{version}` (or `{version_}`, the version with `_` for
`.`) once, plus fixed text around it. spieler only takes upstream tags the template spells, and
the slot only matches integers joined by `.`, `-`, `_` or `+`, so `v{version}` passes over
`v2.1-rc1` and `release-{version}-stable` over the alphas beside it. What the slot holds is the
version. A trailing `*` stands for whatever upstream appends, as in `v{version}-*` for
`v1.2.3-8f3c2a1`, and a bare `*` for tags that do not spell the version at all. Such a tag can not
be spelled from the version, so spieler records the one it found in act.lock. Without `tag` the tag
is the version.

`track` names where new versions come from, a purl without its `pkg:` and with `:` after the
type, as in `github:owner/repo`, `gitlab:group/project`, `pypi:name` or
`generic:name?url=…&regex=…`. A `?branch=` qualifier follows a branch tip instead of tags
(docs/spieler.md). When tracking needs options, `[track]` holds them with `from` for the same
string.

| key          | meaning                                                                 |
| ------------ | ----------------------------------------------------------------------- |
| `from`       | the source, as above                                                    |
| `allow`      | regex a version must match                                              |
| `prerelease` | accept prereleases                                                      |
| `every`      | how old a release must be before it is a candidate, `"30d"`             |
| `base`       | the version an `-unstable-` pin is cut from                             |
| `group`      | update together with other packages of the same group                   |
| `cpe`        | CPE vendor:product for vulnerability lookups                            |
| `frozen`     | why the package never updates, instead of `from`                        |
| `relocks`    | lock ecosystems whose whole table is solved again when this updates     |
| `watch`      | `{ url, regex }` or `{ purl }`, a page or registry listing versions     |
| `locks`      | lock files spieler regenerates, `{ <ecosystem> = "<subdir>" }`          |

### Sources

`fetch` is one source, `[fetch.<name>]` tables several. A source is one of

- `{ url = "…" }`, a url with `{version}`, `{tag}`, `{version_}`, `{major}`, `{minor}` and any
  key of act.lock as placeholders
- `{ asset = "<file>" }`, a release asset of the tracked forge repository at `{tag}`
- `{ archive = true }`, the tracked forge repository's tarball of `{tag}`
- `{ sdist = true }`, the tracked PyPI project's sdist under its PEP 625 name, or
  `{ sdist = "<stem>" }` for an older one named otherwise
- `{ from = "<package>" }`, that package's sources, for a subproject of its tarball

plus `unpack = false` to keep the download as it is, `name` for its store name, and `frozen` (a
reason) to keep its hash when the version moves.

### Building

| key             | meaning                                                                 |
| --------------- | ----------------------------------------------------------------------- |
| `with`          | build systems, in order                                                 |
| `deps`          | packages of the target set the package links or runs                    |
| `tools`         | packages of the build set it runs while building                        |
| `only`          | platform constraints, `{ os = […], abi = […], cross = false, … }`       |
| `patches`       | the patches that apply, in order                                        |
| `[test]`        | `false` to skip tests, or `bin`, `version`, `deps`, `dlopen`, …         |
| `[steps]`       | `after`, `before`, `skip` and `replace` around the systems' phases, or `phases` for all of them |
| `[<system>]`    | a build system's options, `[cmake]`, `[cargo]`, …                       |
| `extras`        | `"python"` gives the interpreter `.env` and `.project`                  |

Every other key is a field of the package record as bigtop reads it (`install`, `exports`, `env`,
`links`, `prebuilt`, `bootstrapTools`, …). Wherever a package is expected a string names one, from
the build set for `tools` and a system's `tool`, from the target set everywhere else. Strings take
the same placeholders as a source url, as in `ldflags = [ "-X main.version={version}" ]`, and these:

| placeholder         | becomes                                                  |
| ------------------- | -------------------------------------------------------- |
| `{pkg:<path>}`      | a target set package, `{pkg:linux-kernel.dev}` an output |
| `{tool:<path>}`     | a build set package                                      |
| `{platform:<path>}` | a platform field, as in `{platform:gnuTriple}`           |
| `{source:<name>}`   | one of the act's sources, unpacked                       |
| `{file:<name>}`     | a file beside the act                                    |

`patches` names every patch that applies, in order, and a `*.patch` beside the act that neither it
nor act.nix names fails evaluation. An entry is a file beside the act, or a pull request or commit url on GitHub, GitLab, Forgejo or Gitea
(`github:owner/repo/pull/123` works too), or any https url serving a diff. `spieler patch` downloads
the url entries beside the act as they are and records which file each became, and the build only
reads those files.

`[on.<when>]` tables merge over the build keys on matching platforms, in the name order of their
conditions. `<when>` is `cross`, `native`, or a value of the platform's `os`, `cpu`, `libc` or
`abi`. `!` negates a condition and `+` joins two, as in `[on."windows+msvc"]` or `[on."!linux"]`.
Tables merge, lists append, anything else replaces. `with` is a Nix keyword, so act.nix spells it
`"with"`.

### Bindings

`[bindings]` names the locked packages of other ecosystems that link this library, so a lock file
that pins one gets the library as a dependency and the library's settings in its build. An
ecosystem (`cargo`, `go`, `python`, `gems`, `hackage`) is a list of names, or a table with
`names` and

- `env`, set in every build that locks one of them, `{root}` being the library's store path
- `tags`, go build tags
- `flags.<gem>`, that gem's extension build flags
- `configSettings.<package>`, that python package's PEP 517 config settings

A name under `flags` or `configSettings` needs no entry in `names`. `[builds.python]` is the same
for a tool, the `names` whose sdist builds with this package plus the `tools` it needs. After an
edit, `motley update rehash` rewrites the act.lock `sys` it changed.

```toml
[bindings]
hackage = [ "zlib", "digest" ]

[bindings.cargo]
names = [ "libz-sys", "libz-ng-sys" ]
env   = { ZLIB_NO_VENDOR = "1" }

[bindings.gems]
flags.zlib = [ "--with-zlib-dir={root}" ]
```

### act.nix

A function of `{ platform, pkgs, tools, toolchain, pin, source, file, fetch, tree, when }` to a
list of changes in the vocabulary above, merged after `[on]`. `pkgs` and `tools` are the target
and build sets, `source <name>` one of this act's sources, `file <name>` a file beside it,
`tree { dir, name, exclude, files }` a directory of the repository, `when <bool> <change>` the
change or nothing.

## act.lock

What spieler last resolved. Hand edits are fine but `spieler apply` rewrites it.

```toml
version = "1.3.2"
date    = "2026-09-09"
hash    = "sha256-tKLefmGqB2HVUMILJ7Gqa0v9GH7fqv64d5S5kK1tcYU="
```

`version`, and `date`, `rev` and `tag` where they apply (`tag` only when act.toml's can not
produce it). One source has `hash`, several a `[hash]` table by name. `sys` lists the set's
libraries the package's lock files link, `[vars]` the other values its urls use, `[patches]`
the file each url entry of `patches` was downloaded as, and `[sources]` the sources an update.nu
`sources` hook named for the pin, `{ url, name }` each, which join act.toml's.
