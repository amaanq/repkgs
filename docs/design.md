# Design

Why motley is shaped the way it is. The README says what it does, [act.md](act.md) how a recipe
reads, [mummer.md](mummer.md) how the compile cache works and [bootstrap.md](bootstrap.md) how the
toolchain is grown.

## Nothing names itself

No output contains its own store path. Everything else follows from that rule.

- Every derivation, bootstrap stages included, is floating content-addressed. A mummer, bigtop or
  toolchain change that leaves the bytes alone rebuilds nothing downstream.
- A closure runs from wherever it is copied to.
- bigtop points `$out` at a scratch prefix inside the build directory, so nothing a build does can
  depend on where it lands. lld hashes the output path into build ids, which would otherwise make
  content addressing unstable.

References to other store objects are relative to the file holding them, and the hash stays in the
string, so the garbage collector, `nix copy` and the reference scanner keep working.

- ELF objects get `$ORIGIN` NEEDED entries from `mummer reloc-fixup`. RUNPATH keeps only libc and
  dlopen directories.
- Mach-O objects are linked with header padding so install names and rpaths can be respelled as
  `@loader_path`, then signed again ad hoc.
- The interpreter is found by a small entry stub (pkgs/cr/crt-interp) that maps ld.so relative to
  `/proc/self/exe`. `prebuilt = true` gives upstream binaries the same stub.
- Scripts are hardlinks to `launch` beside a record with `{root}` placeholders, so nothing patches
  shebangs or writes wrappers.
- glibc finds its data relative to libc.so.6, pkg-config files use `${pcfiledir}`, and
  exports.json values use `{root}`.
- A build installs into a scratch prefix and bigtop moves it into place. Any compiled-in mention of
  the scratch prefix left behind fails the build. Packages that bake their prefix in carry a
  relocatable.patch built on crt-interp's reloc.h.
- Cross builds pass every build-machine dependency to reloc-fixup as `--deny`, so a host path in a
  target output fails loudly.

Debug info is always built and split into a `debug` output laid out by build id, while `out` keeps
its symbol table. Lookup goes by id, so `out` never references `debug` and the split costs no
relocatability.

## Evaluation stays cheap

A package is one directory, read only when its attribute is asked for. There is no callPackage,
no mkDerivation, no module system and no per-package fixpoint. `nix/package` folds a recipe into one
spec, checks it against the schema bigtop emits (nix/schema.json) and produces one derivation whose
builder is `bigtop build`. A misspelt field or a wrongly typed option fails evaluation, not the
build. [eval.md](eval.md) has the numbers.

Every package exists on every platform. `supported` says whether it is meant for this one without
forcing the derivation, so CI filters on a boolean instead of a `tryEval` that would also swallow
real errors. The verdict comes from the act's `only`, a build system that refuses the platform, a
prebuilt act with no source for it, and any unsupported dependency.

The build machine's set is a second import of the same set, so cross compiling is a `platform`
argument and nothing else.

### Overrides

One tree, `<package>.<field>….<verb>`, is the only way to change a package from outside. There is
no `.override`, no overlay and no module beside it.

- The verbs are `set`, `append`, `prepend`, `merge` and `remove`, and `edit` on a package maps the
  whole spec last. They apply in the order set or keep, remove, merge, then prepend and append.
- Several trees join into one before anything applies, so layering costs nothing and a later
  `remove` can undo an earlier `append`.
- Every path is checked. An unknown package, a missing field, a list verb on a set or a dependency
  that names nothing fails evaluation, and the edited spec is validated like a hand-written one.

Features run the other way. A package declares the choices it offers and reads the values chosen,
which keeps a dependency next to the flag that needs it. The default fixes the type, `values`
bounds it, and the package's own override beats the set-wide value.

## Locks are read at build time

Upstream lock files are never copied or rehashed in the tree. A dynamic derivation runs
`mummer fetch <kind>`, which reads the lock inside the build and writes one builtin fetchurl per
artefact carrying the lock's own hash. Evaluation never sees the lock, and no `nix` binary runs in
the sandbox.

- Ecosystems whose locks lack file hashes (Go, Hackage, LuaRocks) keep them in one sorted
  `locks/<ecosystem>.toml` each, merged as a union by git and written by `spieler lock`.
- Native libraries behind locked dependencies are resolved when spieler pins, through
  builder/sys-libs.toml, and recorded as `sys` in act.lock. They become ordinary dependencies that
  evaluation, `supported` and overrides all see. A lock that gains a new `-sys` crate fails the
  build instead of falling back to a vendored copy.
- A source's store name follows its URL, so a version bump with a stale hash fails instead of
  reusing the old download.

## Builders hand over data

One bigtop process prepares the build, runs the phases and finishes the output (debug split,
launchers, relocation, the version check and exports.json). Build systems are Rust, recipe phases
are nushell in a `.nu` beside the act.

A dependency contributes data and never behaviour. Its exports.json lists include and library
directories, pkg-config paths, environment and what propagates, and prepare turns the closure into
search paths and mummer flags. Nothing a dependency ships runs inside another build.

Cross compiling is each build system's job, driven by one platform record (`--host` with
nix/config.site, a meson cross file, a cmake toolchain file, `CARGO_TARGET_*`, `GOARCH`). Tests
run under qemu, or wine for Windows.

Hardening and reproducibility are compiler defaults. mummer adds optimisation, debug info, frame
pointers and the hardening flags outside CFLAGS, where a Makefile cannot drop them, and recipes
switch individual flags off by name.

## The cache sits under Nix

Nix caches whole derivations, so editing one recipe line reruns every compile downstream. mummer
is the only compiler on PATH and asks mummerd for cached objects, links, crates and probe results
when the daemon's socket is mapped into the sandbox. No derivation mentions the cache and outputs
are identical either way, which content addressing lets anyone check. The cache is per user and
machine because whoever writes to it supplies compiler output.

mummerd also hands out build slots, so N sandboxes each running `make -jN` do not oversubscribe
the machine. ghc takes its slots through jape.

## One LLVM for every target

Every platform, Windows and macOS included, is built by the same LLVM, and glibc is the set's libc.
musl only appears in the peanut and stage0. GCC is allowed only early in the x86_64 source
bootstrap. Self-hosting languages take an upstream binary only as a build-only `<lang>-bootstrap`
and build the real one from source.

## Influences

Ekala's EEPs (path as attribute, explicit build systems), Aux tidepool (exports as data, one path
for native and cross), nuenv, Zig (the target as a flag), Spack, Guix, conda (relocation),
wrap-buddy and fzakaria's relocatable ELF work, llm-agents.nix (a declarative updater).
