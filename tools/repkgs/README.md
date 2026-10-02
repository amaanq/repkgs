# repkgs

The command line for working on the package set. With direnv it is on PATH; otherwise
`tools/repkgs/repkgs`. Every subcommand has `--help`, and `--for <platform>` where a platform
makes sense (`aarch64-linux`, `aarch64-macos`, `x86_64-windows-msvc`, …).

`build` and `test` run Nix through ROM, showing the dependency graph and each package's
current phase. Store paths stay on stdout, and a failed build keeps Nix's exit status.
The development shell supplies ROM from the `manic-systems/rom` Git repository's `refactor`
branch, pinned in `flake.lock`. Outside the shell, `rom` must be on PATH.

## Everyday

```console
$ repkgs build jq                     # build, through the compile cache if jigd runs
$ repkgs build --for aarch64-macos jq # cross
$ repkgs test jq                      # jq.tests, or jq itself when it tests in-build
$ repkgs test                         # the builder's own tests, after editing builder/
$ repkgs log jq                       # the last build log
$ repkgs info jq                      # version, platforms, build systems, features (--json)
$ repkgs list --for riscv64-linux --unsupported   # what a platform lacks and why
$ repkgs options cmake                # every cmake.* a package.nix may set, with defaults
$ repkgs fmt                          # treefmt
```

## When a build fails

```console
$ repkgs dev perl                     # or --for aarch64-macos perl
```

drops you into a nu shell with the package's compiler, flags and dependencies. Jib unpacks
and patches the source and runs the same build-system setup as a normal build. Both Rust
build-system phases and inline Nu phases are available through `phase`.

```console
> phases                              # perl.configure autotools.build autotools.install perl.scrub
> phase perl.configure
> phase autotools.build               # fails somewhere in dist/Time-HiRes
> ^make -C dist/Time-HiRes            # go at it directly, edit files, repeat
> exit
$ repkgs dev perl 'phase autotools.build'   # reuse the tree and edits
$ repkgs dev --reset perl             # fresh source, same tree
$ repkgs dev --fresh perl             # remove everything
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

Once the fix is clear, turn the edit into a patch next to package.nix (`diff -u` against a
pristine unpack) and build for real.

## The compile cache

```console
$ repkgs cache start                  # jigd, per user, under /tmp/jigd-<uid>/
$ repkgs cache status
$ repkgs cache stop
```

`build`, `test`, `dev` and `repro` use it when the socket is there. A daemon elsewhere:
`JIG_SOCK=/path/to/socket repkgs …`. Mapping the socket into the sandbox needs your user in
`nix.settings.trusted-users`.

## Adding and updating packages

```console
$ repkgs new foo pkg:github/o/foo 'https://…/foo-{version}.tar.gz'   # pkgs/fo/foo/ with package.toml
$ repkgs update check                 # what is outdated (uptrack)
$ repkgs update apply foo             # new pin and hashes
$ repkgs repro zlib                   # build twice, compare
```

## Rare

```console
$ repkgs seed …                       # rebuild and upload the bootstrap seed
$ repkgs bootstrap ghc riscv64-linux  # cross-build a -bootstrap package's binary and upload it
```

## Files

`repkgs` is the entry point, `nix.nu` what all subcommands share (nix flags, eval helpers, cache
socket), and `dev.nu` drives the development shell. `dev.nix` lets Nix resolve the package's
inputs, including dynamic dependency trees, and export its structured build attributes.
`jib dev prepare` and `jib dev phase` execute the package's build plan in the kept workspace.
`dev-shell.nu` provides the interactive `phases` and `phase` commands.
