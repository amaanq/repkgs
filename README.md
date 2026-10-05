# motley

A package set for unmodified Nix that keeps the store and the language and rethinks what a
derivation holds. The *re-* stands for relocatable, reproducible and another try at nixpkgs.

- **One toolchain.** A single LLVM (clang, lld, compiler-rt, libc++) serves every platform, and
  `--argstr platform riscv64-linux` cross compiles with it rather than with a second compiler.
- **Relocatable outputs.** Binaries locate their libraries relative to themselves, so a store
  path copied anywhere still runs. Upstream prebuilt binaries are treated the same way.
- **Recipes are data.** A package is an `act.toml` saying where its source comes from and how
  it builds, beside the `act.lock` spieler keeps current. Phase code that has to exist is
  nushell, run by bigtop, the builder, with no bash and no setup hooks.
- **A compile cache under Nix.** `cc`, `rustc`, `go` and configure probes are cached by content
  on the machine and shared across derivations. Changing a recipe rebuilds its derivation while
  almost nothing compiles again. The cache is a socket inside the sandbox rather than an input,
  so a `.drv` hashes the same with or without it.
- **Cheap evaluation.** No overlays and no fixpoint per package. Overrides are one tree,
  out-of-tree packages one argument. A package shared with nixpkgs adds about a third of the memory
  and a fifth of the time to evaluation that it does there ([docs/eval.md](docs/eval.md)).
- **A short bootstrap.** A small static peanut reaches glibc in two stages. Rust, Go, Zig, GHC and
  OpenJDK then build from source, each starting from its upstream binary.

[docs/design.md](docs/design.md) gives the reasons and [docs/plan.md](docs/plan.md) what comes
next.

## Trying it

The daemon needs a Nix carrying [NixOS/nix#16459](https://github.com/NixOS/nix/pull/16459).
After a garbage collection, CA rebuilds also go better with
[#16465](https://github.com/NixOS/nix/pull/16465) and [#16477](https://github.com/NixOS/nix/pull/16477).
[nix/nix](nix/nix/default.nix) builds such a Nix (`nix-build nix/nix`, or the flake's
`packages.x86_64-linux.nix`). On NixOS:

```nix
nix.package = inputs.motley.packages.${pkgs.system}.nix; # or import "${motley}/nix/nix" { inherit pkgs; }
nix.settings = {
  experimental-features = [ "nix-command" "ca-derivations" "dynamic-derivations" ];
  system-features = [ "builder-rpc-v0" "big-parallel" "kvm" "nixos-test" "benchmark" ];
};
```

Then:

```console
$ nix-build -A jq                                   # native
$ nix-build -A jq --argstr platform aarch64-linux   # cross
$ nix-build -A ripgrep -A fd -A deno -A pandoc      # cargo, prebuilt, haskell and more
$ nix-build bootstrap -A stage2.x86_64.cc           # the toolchain alone
```

The first build fetches the peanut and builds the toolchain, and later ones are incremental.

`motley`, on PATH through direnv, covers the everyday tasks, all of them listed in
[tools/motley/README.md](tools/motley/README.md):

```console
$ motley build jq                        # with mummerd caching compiles, after `motley cache start`
$ motley build --for aarch64-linux jq    # cross
$ motley test jq                         # builds jq.tests
$ motley dev jq                          # steps through the phases in a kept build tree
$ motley info deno                       # pinned version, build systems, supported platforms
```

## Writing a package

A package is a directory under `pkgs/<first two letters>/<name>/` holding an `act.toml`. `track`
and `fetch` say where the source comes from, and everything else how to build it:

```toml
track = "github:pnggroup/libpng"
fetch = { url = "https://downloads.sourceforge.net/libpng/libpng-{version}.tar.xz" }
with  = [ "cmake" ]
deps  = [ "zlib" ]

[cmake.defs]
PNG_STATIC = false
PNG_TOOLS  = true
```

The version and hash live in the `act.lock` beside it, which spieler writes.
[docs/act.md](docs/act.md) has the whole format.

`with` picks the build systems, which bring their tools, their phases (configure, build, test,
install) and their options, such as `cmake.defs` above, `cargo.features` or `go.tags`. Every build
system also has `<name>.tool` to swap the program itself. bigtop declares the options and
nix/schema.json carries them, so a misspelt field or a wrong type fails evaluation.
`motley options cmake` lists them.

`deps` are linked and come from the target platform's set, `tools` run during the build and come
from the build machine's. Both are found through the usual search paths (`-I`,
`-L`, pkg-config, cmake) without further description. Every `*.patch` beside the act applies after
unpacking, in name order, unless `patches` lists them, and `test.bin = [ "rg" ]` names the
programs when they are not the package's name.

What depends on the platform goes in `[on.<when>]` tables, merged over the rest where they match:

```toml
[on.windows]
deps = [ "libiconv" ]
```

What needs Nix, a fetcher, a local tree or another package's path, goes in an `act.nix` beside it,
a function of the target's facts and the package sets returning a list of changes:

```nix
{ pkgs, ... }:
[
  { cmake.defs.ZLIB_ROOT = "${pkgs.zlib}"; }
]
```

A lock file in the source is used as it is. At build time a dynamic derivation reads it and fetches
each entry as a fixed output with the hash the lock records, for Cargo, Go, npm, pnpm, Yarn,
Bundler, Deno, Hex, Hackage and LuaRocks. go.sum, Hackage and LuaRocks record no usable hashes, so
`locks/*.toml` holds those.

When a build system's phases are not enough, `[steps]` edits its list with `before.<phase>`,
`after.<phase>`, `replace.<phase>` (a phase or a list) and `skip` (a list), or `phases` spells the
whole list out. A phase is a build system's or a function in a nu module beside the act, so
`"rust.configure"` is `configure` in `rust.nu`. Its `ctx` holds the paths and facts of the build:

```toml
[steps]
skip = [ "cargo.test" ]

[steps.replace]
"autotools.configure" = "perl.configure"
```

What is only data needs no phase: `install` copies files into the output and `remove` deletes
paths from it once installed.

Every build ends the same way. ELF outputs are made relocatable, and `bin/<name> --version` runs
in an empty environment and must print the pinned version. A `dlopen` that finds nothing during
that run fails the build unless `test.dlopen = [ "libudev.so.1" ]` marks it optional, and the run
repeats from a copy of the closure under another root. `test.separate = true` moves the test
phase into its own derivation.

The compiler driver supplies hardening and `-O2 -g`, not `CFLAGS`. `cc.hardening.fortify = false`
turns one off and `cc.cflags = [ "-DFOO" ]` (with `cxxflags` and `ldflags`) adds to every compile
whatever the build system. Debug info goes to a separate `debug` output by build-id
(`nix-build -A curl.debug`, then `gdb -iex "set debug-file-directory ./result-debug/lib/debug"`).

Rarer fields: `prebuilt = true` takes upstream binaries and only makes them relocatable,
`completions.bash = [ "completions/foo.bash" ]` (and `zsh`, `fish`, `nu`) installs completions
where each shell looks, `exports.propagate` names a library that users of this one must see too,
like a `Requires:` line in a .pc file, and `exports = false` marks toolchains and applications that
nothing links against.

An out-of-tree package passed through `packages` may still be a `package.nix` calling `package
{ … }` with the same fields, and may declare `features` with a default that a user chooses through
`import ./. { features.crypto = "nettle"; }` or `overrides.<pkg>.features` (nix/features.nix).

## Keeping it current

`spieler` (tools/spieler, also `motley update …`) reads every `act.toml`, asks
upstream for new versions and rewrites its `act.lock`:

```console
$ spieler check          # what is out of date
$ spieler apply zlib     # bump, prefetch, rewrite the hash (--commit makes one commit per bump)
$ spieler lock fzf       # re-pin the Go modules fzf needs in locks/go.toml
```

[docs/spieler.md](docs/spieler.md) has the rest.

## Layout

```
pkgs/xx/<name>/   the packages, xx being the name's first two letters, with act.toml, phase
                  modules (<name>.nu), patches and in-tree sources (src/)
bootstrap/        peanut, stage0 (musl cc), stage1 (llvm), stage2 (a cc per platform)
nix/              evaluation: package/ turns a manifest into a derivation, build-systems.nix binds
                  each `uses` entry, fetch.nix holds the lock-file fetchers
builder/          the nu that phase modules import
locks/            hashes for the ecosystems whose lock files lack them
tests/builder/    a small package per language checking what the builder does (`motley test`)
docs/             design.md (why), mummer.md, spieler.md, plan.md
tools/            the Rust tools (bigtop, jape, mummerd, spieler) and repository scripts
tools/bootstrap/  mummer and tightrope, a workspace of their own since the toolchain builds them
tools/motley/     the command (its README.md is for changing it)
```

The build tools live here too. **mummer** (tools/bootstrap/mummer, Rust) is what `cc` and `rustc` resolve
to in a build: the compiler driver, the cache client and the ELF fixup. **bigtop** (tools/bigtop, Rust)
runs a build from `prepare` through every build system to `finish`. It is on PATH in package
builds only, so editing it leaves the toolchain alone. **mummerd** (tools/mummerd, Rust) is the daemon
behind the socket, holding the cache and the build slots. **launch** and **crt-interp** are the few
hundred bytes of C that let scripts and ELF binaries run from any path. **spieler** (Rust) does
the updates.

The Rust tools build with nixpkgs' toolchain, without bootstrapping the set. The Cargo config links
with Wild through Clang, and both development shells provide those.

```console
$ nix develop .#rust --command cargo build --workspace --manifest-path tools/Cargo.toml
$ nix develop .#rust --command cargo build --workspace --manifest-path tools/bootstrap/Cargo.toml
$ nix develop .#rust --command cargo test --workspace --manifest-path tools/Cargo.toml
```

To try a local daemon through the cache commands, build it and pass its executable:

```console
$ nix develop .#rust --command cargo build --release -p mummerd --manifest-path tools/Cargo.toml
$ motley cache start --daemon tools/target/release/mummerd
$ motley cache status
$ motley cache stop
```

## The bootstrap

```
peanut        static clang, lld, bsdtar, toybox, make and the like
→ stage0    musl, libc++ and mummer, a C and C++ compiler for this machine
→ stage1    glibc, libc++, cmake, then clang and lld for every target, the set's compiler
→ stage2    compiler-rt, the libc and libc++, a cc-<platform> per target built by that clang
→ pkgs/*
→ rust, go, zig, ghc, jdk from their upstream binary as <lang>-bootstrap, then from source
```

`pkgs/pe/peanut/build.nix` reproduces the peanut. `nix build .#bootstrap-llvm -L` builds its LLVM,
Clang and LLD through nixpkgs' minimal x86-64 bootstrap with no Rust or nushell involved, and
[docs/bootstrap.md](docs/bootstrap.md) records the trust boundary of that source build.
`nix build .#bootstrap-nushell -L` starts from Rust 1.90 built by mrustc, builds the following Rust
source releases with that Clang and then a static nu with the last of them. `.#bootstrap-mrustc`
is the starting compiler and `.#bootstrap-peanut` the assembled peanut, and nowhere in that graph is a
downloaded Rust compiler. `nix build .#bootstrap-mummer -L` builds the static mummer with the same
compiler. The first mummer build needs the whole Rust chain, later mummer edits rebuild only mummer.

## License

EUPL-1.2, see [LICENSE](LICENSE). Each patch under `pkgs/` stays under the license of the project it patches.
