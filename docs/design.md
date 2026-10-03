# Design

How the set differs from nixpkgs and why. What is not built yet is in `plan.md`, the updater in
`plumb.md`, and the exact contract of shim and shimd in `shim.md`.

The names that recur: **shim** is the compiler driver (`cc`, `c++` and `rustc` on a build's `PATH`)
and the ELF post-processor, **hoist** the builder that runs a derivation, **shimd** the daemon behind
them both (compile cache, build slots), **launch** the one binary behind every installed script,
**plumb** the updater, and **jsem** the proxy that backs GHC's `-jsem` tokens with shimd's slots.

## Upstream

The last upstream base is manic-systems/manicpkgs commit
`5d574c2d75c1b24419319dddd8f2753d58a4588e`, jj change `ouyumvxn`, dated
2026-09-20, which added the tre package. Nothing after that commit is rebased onto
this stack. Later upstream changes may be borrowed by hand per package.

## Goals

1. **Cheap evaluation**, a package costing a fraction of what it costs in nixpkgs, as bench/eval.nu
   measures on the packages both sets have ([eval.md](eval.md)).
2. **Relocatable outputs.** No output holds its own store path, so every derivation can be
   content-addressed, early cut-off happens by default and a closure runs from any directory.
3. **Rust builders with Nushell phases.** Structured data, real errors, and a seed of a few static binaries.
4. **One toolchain.** A single LLVM targets every platform, and cross compiling is an argument to
   the set.
5. **A compile cache under Nix**, so editing a recipe recompiles what changed and not everything
   downstream of it.

The set uses stock Nix with `ca-derivations` and `dynamic-derivations`, `/nix/store`, no IFD
and nothing fetched at eval time. glibc everywhere, musl only in the seed and stage0. nixpkgs
builds the seed and provides dev tools. The [source bootstrap](bootstrap.md) starts with
x86-64 LLVM, allowing GCC in the early compiler chain. NixOS modules, nixpkgs compatibility
and GCC as a package-set compiler remain outside the scope.

Build machines are x86_64- and aarch64-linux. riscv64, loongarch64, ppc64le, Windows and macOS
are cross targets. Windows supports Microsoft's CRT and SDK through the MSVC ABI, or MinGW
with UCRT and our libc++. macOS uses Apple's SDK and `ld64.lld`. The CPU baseline
(x86-64-v3, armv8.2-a+lse, rv64gc) belongs to the platform and shim adds it, so no package
carries `-march`.

## Evaluation

A package is its `recipe.toml`. nix/package/manifest.nix turns the `[build]` table, with the
changes its `recipe.nix` returns merged on top, into a *spec*, and nix/package checks
that spec against the schema hoist emits (an unknown field, or an option of the wrong type or with
an unknown name, is an error) before making one derivation whose builder is `hoist build`, with a
phase plan and structured JSON attributes. There is no callPackage, mkDerivation or module layer
and no fixpoint per package.

```
pkgs/zl/zlib/recipe.toml     attribute = directory name, sharded by its first two letters
pkgs/cp/cpython314/          another major version is another package, pkgs/aliases.toml maps cpython to it
```

`import ./. { platform }` lists `pkgs/` lazily, so `nix-build -A jq` reads one directory.
`buildPkgs` is the build machine's set, and another platform is another import. An out-of-tree
package passed through `packages` may also be a `package.nix` calling `package { … }`.

Every package exists on every platform, and `pkg.supported` says whether it is meant for it without
forcing the derivation. Prebuilt packages are for the platforms their `recipe.toml` has tarballs
for (keys such as `x86_64-linux` and `aarch64-macos`), a recipe narrows itself with
`targets.{cpu,os,abi,libc}`, `targets.posix = true` or `targets.cross = false`, and an
unsupported dependency makes its dependents unsupported. Only the store paths throw (`bun:
recipe.toml has no 'riscv64-linux' source`), so CI filters on a boolean rather than `tryEval`,
which would hide real errors too.

### Overrides

The set takes one tree of `<package>.<field>….<verb>`:

```nix
import ./. {
  overrides = {
    xz.autotools.flags.prepend = [ "--disable-nls" ];
    curl.dependencies.remove = [ "nghttp2" ];     # names resolve in the final set
    sqlite.env.merge = { SQLITE_DEBUG = "1"; };
    zstd.pin.merge = { version = "1.5.6"; };      # its recipe.toml read again at this version
    zstd.hash.merge = { default = "sha256-…"; };
    ruby.edit = spec: spec // { … };              # anything the verbs cannot express
  };
  packages.openssl-mine = ./openssl-mine;         # out of tree, and may replace an in-tree name
}
```

A list of trees joins into one before anything applies, so ten layers cost what one does, and a
later layer's `remove` takes back what an earlier one appended. Every path is checked (an unknown
package, a `remove` of a missing field, `append` on something that is no list, a dependency naming
nothing) and the result is validated like a written spec. That is the whole mechanism, with no
`.override`, overlays or modules beside it.

`features` run the other way, as choices a package offers under its own names. A package.nix
declares them and reads the values back, so a dependency and the flag that goes with it stay
together:

```nix
{ package, pkgs, features, on }:
package {
  name = "libarchive";
  features = {
    crypto = { values = [ "openssl" "nettle" "none" ]; default = "openssl"; };
    docs = { default = false; };
  };
  dependencies = on (features.crypto != "none") [ pkgs.${features.crypto} ];
}
```

```nix
import ./. {
  features = { docs = false; };                   # every package declaring `docs`
  overrides.libarchive.features = { crypto = "nettle"; };  # libarchive alone
}
```

A feature takes its default's type, and `values` bounds a string or each item of a list, which
suits a choice with more than two answers better than a boolean. The default gives way to the
set-wide argument and that to `overrides.<pkg>.features`. A wrong type, a value outside `values` or
an undeclared name fails evaluation, the result goes into the derivation and reads back as
`pkgs.curl.features`, and a package without features pays nothing for them.

## Sources and lock files

A `recipe.toml` source becomes a fixed-output fetch named after its URL, so a bumped version with
a stale hash fails instead of reusing the old tarball. Archives are unpacked into the store once.

- **Upstream lock files are neither copied nor hashed here.** `fetch.cargoVendor { source }` is a
  dynamic derivation whose producer reads `Cargo.lock` from the fetched source at build time and
  writes one `builtin:fetchurl` per crate with the hash the lock already has, so evaluation never
  sees it. npm, pnpm, Yarn, Bundler, uv, Bun and Deno work the same way. The producer speaks the
  Nix worker protocol itself (in shim), so there is no `nix` in the sandbox and no recursive Nix.
- **Hashes that a lock file lacks** (Go, Hackage, LuaRocks) live in one sorted `locks/<eco>.toml`
  per ecosystem, merged with `merge=union` and filled by `plumb lock`. A package's vendor
  derivation mentions only its own share.
- **Native libraries behind locked dependencies** are settled when the package is pinned. plumb
  reads the lock files in the source it just hashed, looks the names up in `builder/sys-libs.json`
  (openssl-sys to openssl, mattn/go-sqlite3 to sqlite) and writes `sys = [..]` into `[pin]`. The
  package then takes those of them the set has on the platform as ordinary dependencies, which
  evaluation, `info`, `supported` and overrides all see, while ripgrep's manifest never names
  pcre2. The build system checks the lock against `sys`, so a lock that has since gained a -sys
  crate fails with a pointer to `manicpkgs update` rather than building a vendored copy.
- **Autoconf** results that are facts about a platform are pinned in `nix/config.site`.

## Relocatable outputs

An output names other store objects only relative to itself, as in
`$ORIGIN/../../<hash>-dep/lib`. The hash is still in the string, so garbage collection, `nix copy`
and the reference scanner work as they always have.

| reference | made relative by |
|---|---|
| ELF NEEDED and RUNPATH | shim links with a RUNPATH of exactly the directories that satisfied a `-l`. `reloc-fixup` rewrites in place, making NEEDED `$ORIGIN/…/libfoo.so.1` (one `open` per library) and keeping libc and `dlopen` directories in RUNPATH |
| Mach-O LC_LOAD_DYLIB | dependents record each dylib's absolute install name at link time (`-headerpad_max_install_names`), and `reloc-fixup` respells those and store LC_RPATHs as `@loader_path/…` and signs again ad hoc |
| PT_INTERP | a 300-byte entry stub (`crt-interp`) maps ld.so relative to `/proc/self/exe`, with glibc unmodified, in 0.09 ms |
| upstream binaries | with `prebuilt = true`, hoist implant writes the same stub and RUNPATH |
| scripts and wrappers | `launch`, a `bin/foo` hardlink beside a `bin/.foo.launch` record with `{root}` placeholders, instead of shebang patching or makeWrapper |
| glibc data, pkg-config, cmake | relative to `libc.so.6` (one patch), `${pcfiledir}`, and cmake's own relative paths, with `.la` files deleted |
| exported environment | `exports.json` values written with `{root}` |
| compiled-in prefix | the build installs under a scratch prefix that finish moves, and anything left naming it is an error, the reloc.h patch covering openssl's providers |

A build never sees its store path. `$out` is `$NIX_BUILD_TOP/prefix`, finish makes what it knows
relative to the final location, fails when a file still names the prefix and moves the tree into
the store last. So an output's bytes cannot depend on where it lands, which also keeps
content-addressed rebuilds stable, since lld hashes `$out` into build ids and string order.
[NixOS/nix#16465](https://github.com/NixOS/nix/pull/16465) covers derivations that do see `$out`,
and [#16477](https://github.com/NixOS/nix/pull/16477) lets a rebuild that still differs replace a
collected output's build trace rather than fail.

Ambient data such as the CA bundle, zoneinfo and fonts comes from an environment variable or a
system path, never from a store path.

Every package builds with debug info, split into a second output, `debug`, filed by build-id
(`lib/debug/.build-id/ab/cd….debug`) where gdb, lldb, perf, valgrind and systemd-coredump look. `out`
keeps `.symtab`, so backtraces and profiles have names without it. Since `out` never references
`debug` and `debug` is found by id rather than path, the split costs no relocatability.
`debug = false` is for builds that cannot be made to keep DWARF, and an ELF package whose build
produced none fails. Sources are not shipped, DWARF names `/build/source/…`, and `pkg.src` is that
tree.

With no output naming itself, every derivation, bootstrap stages included, is floating
content-addressed. A change to shim, a builder script or the toolchain that leaves a package's bytes
alone lets its dependents resolve to what the store already has instead of rebuilding. Cross builds
pass `--deny <build dependency>` to reloc-fixup, so a build-machine path in a target output fails
rather than becoming a silent reference.

## Builders

One hoist process prepares the environment and source tree, executes the package's phases,
then handles debug splitting, launchers, relocation, version checks and `exports.json`.
Build systems implement the Rust `System` trait. Package-specific Nu phases run in child
processes and hand their environment and build context back to hoist. A package names its build
systems and edits their phases:

```toml
[build]
systems = [ "cmake" ]                # the phases default to the build system's

[build.cmake.defs]
WITH_FOO = true                      # typed options, checked at evaluation

[build.after]
"cmake.install" = [ "foo.gen" ]      # foo.gen is `gen` in foo.nu beside recipe.toml
```

- **Dependencies contribute data and never behaviour.** Each output carries an `exports.json` of
  include, lib and pkg-config directories, environment and propagation. `prepare` turns the closure
  into search paths and the flags shim adds, and nothing a dependency ships runs in another build.
- **Two dependency lists**, `tools` for the build machine, on `PATH`, and
  `needs` for the target.
- **Cross compiling belongs to the build system**, driven by one platform record: `--host` and
  `config.site`, a meson cross file, a cmake toolchain file, `CARGO_TARGET_*`, `GOARCH`. Tests run
  under qemu.
- **Hardening and reproducibility are compiler defaults.** shim adds `-O2 -g`, frame pointers and
  the nixpkgs hardening set outside `CFLAGS`, so no Makefile drops them. hoist's typed hardening
  table defines the flags, a platform or package turns names off (`cc.hardening.fortify = false`).
  `SOURCE_DATE_EPOCH`, prefix maps and fixed seeds take care of reproducibility.
- **Tests run**, inside the build or as `<pkg>.tests` with `tests.separate`. Every `bin/x
  --version` has to print the pinned version from an empty environment, under an audit module that
  fails when a `dlopen` finds nothing.

## shim and shimd

Nix caches whole derivations, so one changed line in a recipe reruns every compiler call
downstream of it. shim is the only compiler on `PATH`, and when `/nix/var/nix/shimd/socket` is
mapped into the sandbox (`extra-sandbox-paths`) it asks shimd before compiling. No derivation
mentions the cache, so outputs are the same either way, which CA outputs let anyone check.

| cached | key |
|---|---|
| `cc -c` objects, probe links, real links (ELF) | compiler path, arguments, environment and source bytes, then the headers and libraries read through `-MD` and lld's dependency file. The package's own `$out` hash is masked in keys and cached bytes and restored on replay |
| compile failures | replayed when every input is known, which covers most of configure |
| rustc crates, Go actions, Haskell units | Rust dep-info, `--extern` and standard library identities, metadata flags and `env!` dependencies. Go's action IDs and cabal's unit id |
| `config.cache`, cmake probe results, tool cache dirs | source and patches, script paths and contents, complete dependency paths, platform, environment and build-system options |

Configure caches are reused for matching package inputs. Changing a dependency's store hash,
compiler flags, source files or `CONFIG_SITE` contents invalidates them. Cache availability and
compiler log destinations do not affect the key. CMake definitions and generator choices are
included, and cached strings are restored literally.

Autoconf C header checks first reuse successful, warning-free compiler results after validating
their dependency manifests. These hits avoid launching the preprocessor. Otherwise, header
checks can share successful results across packages. Shim preprocesses
the current test and keys the result by the compiler, expanded flags, warning policy, helper
definitions, site configuration, compiler environment, source, preprocessed output and headers.
It replaces package metadata values in the source key only when Clang reports those macros as
unused. Used metadata and feature macros remain part of the key. Mutable headers are hashed
on every lookup, and immutable headers retain their full store paths.

Configure prints `(shared)` for these hits. Missing headers, unexpected preprocessing warnings,
unsupported compiler options and an unavailable daemon fall back to the original check.
Only the `cc`, `gcc`, `cc-build` and `gcc-build` aliases of the same shim installation are supported.
Failed checks, link checks and runtime checks are not shared through this mechanism. `nix/config.site`
still supplies common platform defaults, and arbitrary `ac_cv_*` names are never used as
cross-package keys.

`tools/benchmark-configure.nu` measures Autoconf configuration with the selected seed and
the packages' resolved build inputs. It starts a fresh daemon, primes the compiler cache,
alternates runs with and without shared header results, then measures whole `config.cache`
reuse. Every pass starts with a fresh source and build directory at the same paths. The
benchmark rejects changes to the generated `Makefile` and `config.h` files, requires the warm
configure cache to report a restore, and preserves the per-run logs and timings in the printed
directory. `environment.json` records the seed and daemon identity, while `summary.json` records
the median timings and configure-cache speedup. Package builds finish before measurements begin.

```console
nu tools/benchmark-configure.nu --seed ./result-bootstrap-seed --daemon /path/to/shimd sed diffutils
```

Measured on x86-64 Linux with the source-built seed and release shimd on September 13, 2026.
Compiler-cache and shared-header times are medians of three interleaved configure runs.
The final column measures a restored whole configure cache, with identical generated files.

| package | compiler cache | shared headers | configure cache |
|---|---|---|---|
| sed 4.10 | 8.29 s | 8.34 s | 0.34 s |
| diffutils 3.12 | 10.34 s | 10.37 s | 0.29 s |

The header fast path avoids 48 preprocessor launches for sed and 42 for diffutils.
Whole configure-cache reuse gives the larger reduction, about 25× and 35× respectively.
Nix output checks also pass for both packages with cold and warm caches. In ordinary sandboxed
builds, diffutils reuses 16 successful header checks from sed. Warm rebuilds restore the whole
configure cache and reuse 97 percent of sed's compiler actions and 98 percent of diffutils'.

shimd keeps an append-only object store of packs with an in-memory index, evicts whole packs and
serves hits with `sendfile`. It remembers store-file identities so a hit does not hash headers
again, and it hands out build slots so N sandboxes running `make -jN` do not oversubscribe the
machine (per process for cc and rustc, through `-toolexec` for go and through `jsem` for ghc). A
warm cache took sqlite3.c from 82 s to 0.08 s and fd's 200 rlibs from 218 s to 1.4 s.
Request keys, manifests and the pack format are specified in `shim.md`. A cache writer can supply
compiler artifacts, so the cache stays per user and machine.

## Toolchain, bootstrap, cross

```
seed      static musl: LLVM multicall (clang, lld, llvm-ar …), bsdtar, toybox, dash, make, python
→ stage0  musl headers, compiler-rt, musl, linux headers, libc++, shim, cc     (this machine, seed clang)
→ stage1  the same with glibc, cc-boot, cmake, llvm with clang, lld, libLLVM.so  (this machine, seed clang)
→ stage2  linux headers, glibc, compiler-rt, libc++, cc-<platform>             (per target, stage1's clang)
→ pkgs/*
```

Recipes run through `shim bootstrap` from `shim/src/bootstrap/`. musl, compiler-rt and the C++ runtimes compile from file
lists without cmake. The source-built Rust chain compiles static shim before stage0 installs it.
The one generated input is compiler-rt's builtins list per cpu (pkgs/ll/llvm/update.nu). What
differs per target follows the object format (`platform.binfmt` of elf, macho or coff), and a libc
or SDK tells compiler-rt and cc its header directories and driver flags through `etc/cc/` files.
RISC-V needed `-mno-relax`.

A self-hosting language has its upstream binary as `<lang>-bootstrap`, prebuilt, relocated and
only ever a build dependency, and `<lang>` built from source with it (Go, Rust, GHC, OpenJDK). Zig
bootstraps from the WASM blob in its source. The seed comes from `pkgs/se/seed/build.nix`, through
nixpkgs' static packages today and from this set eventually.

## Influences

Ekala's EEPs (path as attribute, explicit build systems), Aux tidepool (exports as data, one path
for native and cross), nuenv, Zig (the target as a flag), Spack, Guix and conda (relocation),
wrap-buddy and fzakaria's work on relocatable ELF, and llm-agents.nix (a declarative updater).
