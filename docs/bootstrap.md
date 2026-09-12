# Source bootstrap

The first target is x86-64 Linux. The source seed build connects LLVM, mrustc, successive Rust
releases and Nushell without downloading a Rust compiler. Its compiler ancestry reaches the
229-byte hex0 executable through nixpkgs' minimal bootstrap.

## LLVM

The pinned nixpkgs already uses its minimal bootstrap for the x86-64 C and C++ toolchain. Its
entry point is the 229-byte AMD64 hex0 executable. The chain builds Mes, TinyCC, successive
GCC versions and the tools needed by the normal nixpkgs build environment.

`pkgs/se/seed/build.nix` uses that environment to build our pinned LLVM as a static musl
multicall binary. It includes Clang, LLD and the object tools used by repkgs. LLVM source
extraction uses the C and shell tools from nixpkgs. CMake, Python and Ninja are selected
without a Rust or Nushell dependency.

```console
nix build .#bootstrap-llvm --no-link --print-out-paths -L
nu bootstrap/audit.nu > /tmp/repkgs-bootstrap-inputs.json
```

The audit walks build dependencies, including fixed-output fetchers. It rejects Rust, Cargo,
Nushell, jig and the old binary bootstrap-tools bundle in the LLVM graph, and requires
hex0-seed to be the only executable builtin fetch. Its JSON output records the hashes and
URLs of fixed-output inputs for inspection. Run it after changing the nixpkgs pin or LLVM
build dependencies.

These checks identify dependency mistakes. They do not inspect the contents of source
archives or prove that a compiler is correct. The source hash in `pkgs/ll/llvm/sources.toml`
is shared with the normal package set.

## Trust boundary

The host kernel, Nix and the machine running them remain trusted. Nix also needs the pinned
stage0 source tree from its store or a substituter before it can build the first assembler.
That tree contains the bootstrap sources, with seed executables fetched separately.

Normal builds may substitute compiled dependencies from the configured caches. The recorded
derivations describe their source ancestry, but a cache hit does not verify that ancestry by
rebuilding it. A build from hex0 needs a separate empty store, the pinned stage0 sources and
source downloads, with compiled substitutes disabled.

The nixpkgs chain still accepts generated files in release archives, including some configure
scripts and parsers. Regenerating those from their original inputs is further work before
claiming the stricter live-bootstrap standard.

## Rust and the seed

`bootstrap-mrustc` builds Rust 1.90 and Cargo using mrustc, then rebuilds the compiler and its
standard library with Rustc. `pkgs/se/seed/rust-chain.toml` pins the intermediate source
releases from 1.91 through 1.97. Each builds its host compiler, Cargo and standard library
using the preceding release and LLVM 21. Each completed release is a separate Nix output.

The first handoff builds stage2 because mrustc's output includes private compiler libraries
that conflict with the next release's stage1 Rustdoc build. Later intermediate releases
use stage1 builds from the clean installed toolchains.

`bootstrap-rust-stage0` is the last compiler in that chain. `bootstrap-rust` uses it to build
our pinned Rust source release. The source-built Clang compiles the matching LLVM backend
included in the final Rust release. That build produces a stage2 Rust compiler, Cargo and
standard libraries for the GNU host and the musl target.

`bootstrap-nushell` uses that compiler to build our pinned Nushell source as a static musl
executable. Its features match the normal package recipe. Shell and Python builders handle
the steps before Nu exists. The remaining static userland and C runtime libraries still come
from nixpkgs' source recipes.

```console
nix build .#bootstrap-rust -L
nix build .#bootstrap-nushell -L
nu bootstrap/audit.nu .#bootstrap-nushell --rust > /tmp/repkgs-bootstrap-inputs.json
nix build .#bootstrap-seed -L
```

The Rust audit checks the intermediate source URLs and hashes against the manifest and
rejects Rust binary toolchains in the build dependencies. Nix generates the bootstrap config
directly, since its general TOML converter needs Rust. The normal package set still uses
`pkgs/ru/rust-bootstrap/sources.toml`. The source seed build does not depend on that manifest.

Seed assembly takes Nu from this build and strips its binaries with the source-built LLVM.
It removes store references from the tool binaries and copied data, including Python's build
configuration. Nix rejects the seed if it retains a dependency on another store path.
These targets do not replace the downloaded seed used by normal repkgs builds. Publishing a
new seed remains separate work.

`bootstrap-jig` builds the Rust compiler wrapper with the same source compiler and static musl
target. Normal bootstrap recipes copy this binary into the cache wrapper and each configured
toolchain. Its dependencies use the nixpkgs revision pinned in `flake.lock`, including for
non-flake builds. Editing jig rebuilds this small binary without restarting the compiler chain.
Seed assembly includes it alongside Nu.

Once assembled, the seed can be stored and reused for ordinary package builds. Rebuilding
the seed and checking its inputs remains separate from rebuilding an application.

## Building the Rust chain

The pinned nixpkgs provides mrustc 0.12 and a Rust 1.90 bootstrap using LLVM 20. Build and
inspect that starting point with the separate target, or select an intermediate compiler.

```console
nix build .#bootstrap-mrustc -L
nu bootstrap/audit.nu .#bootstrap-mrustc --mrustc > /tmp/repkgs-mrustc-inputs.json
nix build .#bootstrap-rust-1_91_1 --cores 30 --max-jobs 1 -L
nix build .#bootstrap-seed --cores 30 --max-jobs 1 --keep-failed -L
```

Rust's supported bootstrap path uses the preceding minor release. The compiler uses unstable
features and internals that older compilers cannot reliably build. Patch releases can be
skipped. Our Nushell pin requires at least Rust 1.95, while the final seed compiler follows
the repository's Rust pin.

The assembled seed still contains the tools needed for ordinary package builds. Reducing its
compiler trust roots to hex0 does not make that runtime tool bundle 229 bytes or remove the
host and generated-source assumptions above.

## References

- [nixpkgs minimal bootstrap](https://github.com/NixOS/nixpkgs/tree/422d1ae605d7fcd9896412b37dc065c456c0eefb/pkgs/os-specific/linux/minimal-bootstrap)
- [stage0 source preparation](https://github.com/NixOS/nixpkgs/blob/422d1ae605d7fcd9896412b37dc065c456c0eefb/pkgs/os-specific/linux/minimal-bootstrap/stage0-posix/make-bootstrap-sources.nix)
- [live-bootstrap](https://github.com/fosslinux/live-bootstrap)
- [mrustc bootstrap repair](https://github.com/NixOS/nixpkgs/pull/517833)
