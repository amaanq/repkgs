# motley

The command for working on the package set, on PATH through cade and otherwise at
`tools/motley/motley`. Each subcommand has `--help`, and those where a platform matters take
`--for <platform>` (`aarch64-linux`, `aarch64-macos`, `x86_64-windows-msvc` and the rest).

`build` and `test` run Nix through ROM, showing the dependency graph and each package's
current phase. Store paths stay on stdout, and a failed build keeps Nix's exit status.
The development shell supplies ROM from the `manic-systems/rom` Git repository's `refactor`
branch, pinned in `flake.lock`. Outside the shell, `rom` must be on PATH.

## Day to day

```console
$ motley build jq                     # through the compile cache when mummerd runs
$ motley build --for aarch64-macos jq # for another platform
$ motley test jq                      # jq.tests, or jq when it tests inside its build
$ motley test                         # tests/builder, after a builder/ or bigtop edit
$ motley log jq                       # its most recent build log
$ motley info jq                      # version, platforms, build systems and features (--json)
$ motley list --for riscv64-linux --unsupported   # what a platform lacks, and why
$ motley options cmake                # every cmake.* option, with defaults
$ motley fmt                          # nuff, --check to only check
```

## When a build fails

```console
$ motley dev perl                     # --for aarch64-macos picks another platform
```

drops you into a nu shell with the package's compiler, flags and dependencies. Bigtop unpacks
and patches the source and runs the same build-system setup as a normal build. Both Rust
build-system phases and inline Nu phases are available through `phase`.

```console
> phases                              # perl.configure autotools.build autotools.install perl.scrub
> phase perl.configure
> phase autotools.build               # say it stops in dist/Time-HiRes
> ^make -C dist/Time-HiRes            # work on it directly, edit, try again
> exit
$ motley dev perl 'phase autotools.build'   # reuse the tree and edits
$ motley dev --reset perl             # a fresh source in the same tree
$ motley dev --fresh perl             # start over completely
```

The tree lives under `$XDG_CACHE_HOME/motley/dev/<pkg>-<platform>/`, falling back to
`~/.cache`. `source/` holds your edits, `build/` holds build artifacts, and `prefix/` receives
installed files. Re-entry preserves phase environment changes and skips setup when the
inputs and cache settings match. Changed inputs rerun setup while keeping your source and
build directories. `--reset` discards those directories and `--fresh` removes the whole
workspace. The shell can access the network and the host's `/usr`, so confirm fixes with
`motley build`. Use `--reset` to unpack a changed source version or apply changed patches.

The peanut and Rust toolchain flags work here too.

```nu
motley dev --peanut ./result-bootstrap-peanut --rust-toolchain ./result-bootstrap-rust ripgrep
```

Once the fix is clear, make the edit a patch beside act.toml (`diff -u` against an untouched
unpack), which applies by default, and build for real.

## The compile cache

```console
$ motley cache start                  # mummerd for this user, under /tmp/mummerd-<uid>/
$ motley cache status
$ motley cache stop
```

`build`, `test`, `dev` and `repro` use the socket when it is there, and `MUMMER_SOCK=/path/to/socket
motley …` points them at another daemon. Mapping the socket into the sandbox takes a user listed
in `nix.settings.trusted-users`.

## New packages and updates

```console
$ motley new foo pkg:github/o/foo 'https://…/foo-{version}.tar.gz'   # pkgs/fo/foo/ with act.toml
$ motley update check                 # what is out of date (spieler)
$ motley update apply foo             # a new pin and hashes
$ motley repro zlib                   # build again and compare
```

## Seldom

```console
$ motley peanut …                       # build and upload the bootstrap peanut
$ motley bootstrap ghc --cpus riscv64 # cross build a -bootstrap package's binary and upload it
```

## Files

`motley` is the entry point and `nix.nu` holds what every subcommand shares, the nix flags, the
evaluation helpers and the cache socket. `dev.nu` drives the development shell. `dev.nix` lets Nix
resolve the package's inputs, including dynamic dependency trees, and export its structured
build attributes. `bigtop dev prepare` and `bigtop dev phase` execute the package's build plan in the kept workspace.
`dev-shell.nu` provides the interactive `phases` and `phase` commands.
