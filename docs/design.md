# Design

This doc explains why motley is built the way it is.

The mechanics live in [act.md](act.md), [nix-layer.md](nix-layer.md), [mummer.md](mummer.md), [spieler.md](spieler.md) and [bootstrap.md](bootstrap.md).

## Relocation

Builds install into a scratch prefix that bigtop moves into place, and references to other store
paths are written relative to the file holding them, with the hash left in the string for the
garbage collector and `nix copy`. Since no output refers to its own store path, every derivation can
be floating content-addressed, so a mummer or bigtop change that leaves the bytes alone rebuilds
nothing downstream, and a closure runs wherever it's copied. The cost of this an entry stub for the
ELF interpreter (pkgs/cr/crt-interp), launch records instead of shebangs, and a relocatable.patch
for each package that bakes its prefix in. Debug info lives in a separate `debug` output that
debuggers find by build id, so `out` never references it and can be copied without it.

## Evaluation

Evaluating a package reads only its own directory. The recipe there becomes a spec that's checked
against the schema bigtop emits, so a misspelled field fails evaluation instead of the build, with
no callPackage, mkDerivation or module system in between. Changes from outside go through the
override tree, where every path is checked, instead of `.override` and overlays. Cross compiling is
a `platform` argument, because the build machine's set is a second import of the same set. A package
shared with nixpkgs costs about a third of the memory and a fifth to a quarter of the time to
evaluate that it does there ([eval.md](eval.md)).

## Builds

bigtop runs the whole build in a single process and drives autotools, cmake, meson, cargo and the
other build systems from its own Rust modules, and a recipe only writes nushell for the steps those
don't cover. Each dependency lists its include and library directories, pkg-config paths and
environment in an exports.json, and bigtop turns those into search paths. There are no setup hooks,
so a dependency can't run code in the builds that use it. mummer adds the hardening flags, frame
pointers and debug info to every compile itself, so a Makefile that overwrites CFLAGS can't drop
them, and a recipe turns one off by name with `cc.hardening`.

## Lock files

A dynamic derivation reads each upstream lock inside the build and writes a builtin fetchurl per
artifact with the lock's own hash, so evaluation never parses a lock and nothing gets rehashed into
the tree. Go, Hackage and LuaRocks locks lack file hashes, so those live in `locks/`. A source's
store name follows its URL, so bumping a version without updating its hash fails the fetch.

## Compile cache

Nix caches whole derivations, so editing a recipe reruns every compile downstream of it. mummer is
the only compiler on PATH, and it asks mummerd for cached objects, links, crates and probe results
when the daemon's socket is mapped into the sandbox. The cache isn't an input, so a `.drv` hashes
the same with or without it, and a cached build lands on the same content-addressed store path as an
uncached one. Each user on each machine gets their own cache, because whoever can write to it
decides the compiler output later builds get. Since every build already talks to mummerd, it also
hands out the build slots that keep parallel sandboxes from oversubscribing the machine.

## Toolchain

Every platform is built by the same LLVM, and glibc is the set's libc, with musl only in the peanut
and the starter that build the way up to it. Rust, Go, Zig, GHC and OpenJDK need an older copy of
themselves to compile, which that LLVM can't provide, so each starts from upstream's binary as a
build-only `<lang>-bootstrap` and builds its real compiler from source.
