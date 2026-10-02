# Plan

The work still ahead. design.md covers what exists.

## Source bootstrap

The x86-64 source bootstrap has completed in an isolated store without compiled substitutes.
All 13 checkpoints through LLVM, mrustc, Rust 1.98.1, Nushell, jig and seed assembly match the
normal-store builds byte for byte. The assembled seed has no store references and runs after
relocation. The complete source archive restores offline into an empty store, and the 49 early
stages through kaem rebuild there with a matching kaem output. A full replay on another machine
and regenerating release archives' generated files remain unverified.
The dependency boundary and rebuild commands are in [bootstrap.md](bootstrap.md).

## Compilers we still download prebuilt

An upstream binary may only ever be `<x>-bootstrap`, a build dependency of an `<x>` built from
source, as go, rust, zig and jdk already are. These remain:

- **bun**, through zig, our clang and lld and cmake, with its vendored WebKit and JavaScriptCore
  built by our toolchain and its JavaScript parts through `fetch.bunDeps`. Around an hour to build.
- **deno**, a cargo build whose weight is `rusty_v8`, which should build v8 with gn, ninja and our
  clang instead of taking the prebuilt static library. `denort` falls out of the same build.
- **node**, by replacing the remaining bundled dependencies with ours wherever configure allows.

Until then they serve as build tools only, pinned per cpu and never linked into an output. On
riscv64 bun, deno and node have no upstream binary and do not cross build yet (`repkgs bootstrap
<x>` uploads ours once they do), and `ghc-bootstrap` is Debian's package until upstream ships a
bindist or hadrian can build a compiler for another machine.

## Platforms

- **The seed** on loongarch64 and powerpc64le end to end, whose backends are already in, and built
  from this set's own static packages rather than nixpkgs, checked as a fixed point in CI.
- **Windows**: `.exe` and `.dll` install names, the cargo and go targets, and wine as the test
  emulator. autotools stays unsupported there.
- **FreeBSD and NetBSD** use ELF and clang upstream, so crt_interp, `$ORIGIN` and the launchers
  carry over. The libc comes from a release's `base.txz` first and its `src.txz` later. Without a
  user-mode emulator, tests need a VM.
- **macOS**: our own libc++ with `@rpath` install names, launchers for Mach-O scripts, an SDK
  pruned of the libraries this set builds, and a hop to a darwin builder for tests.
- **wasm32-wasi** as another cross platform, with wasi-libc for glibc and no launcher.

## Ecosystems

An ecosystem is an interpreter package, a build system, a lock-file producer and, only where the
upstream lock has no usable hash, a `locks/<registry>.toml`. Roughly by value for the effort:

| ecosystem | where it stands |
|---|---|
| Perl CPAN | perl is in, and `cpanfile.snapshot` has no hashes, so `locks/cpan.toml` |
| JVM (gradle, maven) | jdk is in. gradle's `verification-metadata.xml` or maven into an offline `~/.m2`, with gradle the hard part |
| .NET | a prebuilt SDK, built from source later, and `packages.lock.json`'s sha512 into an offline NuGet feed |
| PHP | php from C, and `composer.lock` often lacks shasums, so `locks/packagist.toml` |
| OCaml | from C, opam locks without hashes into a table, dune builds |
| R | from C and Fortran through flang, and `renv.lock` has hashes |
| luajit | `HOST_CC=cc-build` and `CROSS=` for cross builds |

Swift (its own LLVM fork), Dart and Flutter, Julia, Nim, Crystal and D are not planned. Yarn berry
stays out because its checksum covers a zip yarn repacks, not the registry tarball.

**Native dependencies for JS locks.** sys-libs.json covers cargo, go, python, gems. node-gyp
addons (`sharp` with libvips, `better-sqlite3`, `canvas` with cairo and pango) need the same table
for npm, pnpm and bun, and `uptrack check` should warn when a lock names a library the set lacks.

## Infrastructure

- **CI**: builds on both build platforms and every cross target, a harmonia cache with
  realisations, and `repkgs repro` as a job.
- **uptrack**: the reports and the GitHub sync docs/uptrack.md lists as not built.
- **builder/ reach**: every package's script hashes the whole `builder/` tree, so an edit to any
  module there rebuilds everything down to llvm. Only what every script imports should live there.
- **llvm install size**: `install` copies the roughly 200 component archives libLLVM.so was linked
  from, 807 MB of .a beside a 57 MB .so and more for llvm21 with every target, and only zig's lld
  links them. `LLVM_DISTRIBUTION_COMPONENTS` with `install-distribution` installs a named list and
  writes a matching LLVMExports.cmake. Due with the next full rebuild.

## jig

- Measure cold builds and cache replay with the Rust wrapper. Its request keys keep exact
  arguments and compiler paths, so the C++ wrapper's benchmark results do not carry over.
- Link shapes that still log as `linked` or `uncacheable` (libtool relinks, `@rsp` files, lines
  with several sources), bin and proc-macro crates in rustc mode, and rustc keys that change when
  only the vendor store path does. A recurring `compiled-error` across identical rebuilds points
  at an unstable conftest key.
- Should llvm-sized links show up in profiles, hash their inputs in parallel.

## Builder

- **.pyc files in python outputs**: find what pip and meson installs write (`__pycache__` with
  source paths and mtimes) and either delete them or compile again with `--invalidation-mode
  unchecked-hash`.
- **cmake's `MSVC` on windows**: cmake sets `MSVC` for cl-style drivers only, and our cc is a
  GNU-style clang targeting msvc, so projects branching on `if(MSVC)` (xz's manifests, jsoncpp's
  static suffix, libcpuid's masm) go the wrong way. A clang-cl personality for cmake builds on
  windows or `CMAKE_C_SIMULATE_ID` would fix it, and until then those packages are for linux and
  macos.
- **config.sub**: old autotools tarballs reject riscv64 and loongarch64 triples, so a cross
  `autotools.configure` should copy current config.sub and config.guess in once a package needs it.
- **toolchain references**: `reloc-fixup --deny` the `cc` wrapper in native builds too, after
  finding which outputs record their compiler (python's sysconfig, perl's Config.pm) and deciding
  what those should say instead.
- **debug through collectors**: a dynamic-derivation collector (cargo and cabal packages) forwards
  `out` only, so the inner build's `debug` output is lost.
- **rust cross**: std for the target triple from our llvm, and cargo's linker per target.
- `dep-root --optional` and a lint for dep-root names missing from dependencies, lock validation
  in uptrack at update time, `repkgs fetch-plan` for what a build would download and `repkgs debug
  <pkg>` for gdb pointed at the package's debug output.

## Known test gaps

- m4: gnulib's `test-float-h.c` does not compile with clang in gnu23 mode (C23
  `*_IS_IEC_60559`), and `test-posix_spawn-chdir` once spun forever in the sandbox.
