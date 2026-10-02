# repkgs

The command for working on the package set, on PATH through direnv and otherwise at
`tools/repkgs/repkgs`. Each subcommand has `--help`, and those where a platform matters take
`--for <platform>` (`aarch64-linux`, `aarch64-macos`, `x86_64-windows-msvc` and the rest).

`build` and `test` run Nix through ROM, showing the dependency graph and each package's
current phase. Store paths stay on stdout, and a failed build keeps Nix's exit status.
The development shell supplies ROM from the `manic-systems/rom` Git repository's `refactor`
branch, pinned in `flake.lock`. Outside the shell, `rom` must be on PATH.

## Day to day

```console
$ repkgs build jq                     # through the compile cache when jigd runs
$ repkgs build --for aarch64-macos jq # for another platform
$ repkgs test jq                      # jq.tests, or jq when it tests inside its build
$ repkgs test                         # tests/builder, after a builder/ or jib edit
$ repkgs log jq                       # its most recent build log
$ repkgs info jq                      # version, platforms, build systems and features (--json)
$ repkgs list --for riscv64-linux --unsupported   # what a platform lacks, and why
$ repkgs options cmake                # every cmake.* option of [build], with defaults
$ repkgs fmt                          # nuff, --check to only check
```

## When a build fails

```console
$ repkgs dev perl                     # --for aarch64-macos picks another platform
```

drops you into a nu shell with the package's compiler, flags and dependencies. Jib unpacks
and patches the source and runs the same build-system setup as a normal build. Both Rust
build-system phases and inline Nu phases are available through `phase`.

```console
> phases                              # perl.configure autotools.build autotools.install perl.scrub
> phase perl.configure
> phase autotools.build               # say it stops in dist/Time-HiRes
> ^make -C dist/Time-HiRes            # work on it directly, edit, try again
> exit
$ repkgs dev perl 'phase autotools.build'   # reuse the tree and edits
$ repkgs dev --reset perl             # a fresh source in the same tree
$ repkgs dev --fresh perl             # start over completely
```

The tree lives under `$XDG_CACHE_HOME/repkgs/dev/<pkg>-<platform>/`, falling back to
`~/.cache`. `source/` holds your edits, `build/` holds build artifacts, and `prefix/` receives
installed files. Re-entry preserves phase environment changes and skips setup when the
inputs and cache settings match. Changed inputs rerun setup while keeping your source and
build directories. `--reset` discards those directories and `--fresh` removes the whole
workspace. The shell can access the network and the host's `/usr`, so confirm fixes with
`repkgs build`. Use `--reset` to unpack a changed source version or apply changed patches.

The seed and Rust toolchain flags work here too.

```nu
repkgs dev --seed ./result-bootstrap-seed --rust-toolchain ./result-bootstrap-rust ripgrep
```

Once the fix is clear, make the edit a patch beside recipe.toml (`diff -u` against an untouched
unpack), list it in `patches` and build for real.

## The compile cache

```console
$ repkgs cache start                  # jigd for this user, under /tmp/jigd-<uid>/
$ repkgs cache status
$ repkgs cache stop
```

`build`, `test`, `dev` and `repro` use the socket when it is there, and `JIG_SOCK=/path/to/socket
repkgs …` points them at another daemon. Mapping the socket into the sandbox takes a user listed
in `nix.settings.trusted-users`.

## New packages and updates

```console
$ repkgs new foo pkg:github/o/foo 'https://…/foo-{version}.tar.gz'   # pkgs/fo/foo/ with recipe.toml
$ repkgs update check                 # what is out of date (uptrack)
$ repkgs update apply foo             # a new pin and hashes
$ repkgs repro zlib                   # build again and compare
```

## Seldom

```console
$ repkgs seed …                       # build and upload the bootstrap seed
$ repkgs bootstrap ghc --cpus riscv64 # cross build a -bootstrap package's binary and upload it
```

## Files

`repkgs` is the entry point and `nix.nu` holds what every subcommand shares, the nix flags, the
evaluation helpers and the cache socket. `dev.nu` drives the development shell. `dev.nix` lets Nix
resolve the package's inputs, including dynamic dependency trees, and export its structured
build attributes. `jib dev prepare` and `jib dev phase` execute the package's build plan in the kept workspace.
`dev-shell.nu` provides the interactive `phases` and `phase` commands.
