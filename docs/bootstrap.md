# Source bootstrap

The first target is x86-64 Linux. Build LLVM, Rust and Nushell with hex0 as the only initial
compiler executable. The current Rust and Nu path still uses a pinned upstream Rust stage0.
The separate mrustc build is the starting point for removing it.

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

`bootstrap-rust` builds the pinned Rust source release using three upstream stage0 components
from `pkgs/ru/rust-bootstrap/sources.toml`. Those are rustc, Cargo and the host standard library.
The source-built Clang compiles the matching LLVM backend included in Rust's source release.
The build produces a stage2 Rust compiler, Cargo and standard libraries for the GNU host and
the musl target.

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

The Rust audit permits the three stage0 downloads and checks their URLs and hashes against
the manifest. It rejects an additional nixpkgs Rust toolchain in the build dependencies.
Nix generates the bootstrap config directly, since its general TOML converter needs Rust.

Seed assembly takes Nu from this build and strips its binaries with the source-built LLVM.
It removes store references from the tool binaries and copied data, including Python's build
configuration. Nix rejects the seed if it retains a dependency on another store path.
These targets do not replace the downloaded seed used by normal repkgs builds. Publishing a
new seed and porting the jig wrapper to Rust remain separate steps.

Once assembled, the seed can be stored and reused for ordinary package builds. Rebuilding
the seed and checking its inputs remains separate from rebuilding an application.

## Removing the Rust binary

The pinned nixpkgs provides mrustc 0.12 and a Rust 1.90 bootstrap using LLVM 20. Its dependency
graph reaches hex0 without a downloaded Rust compiler. Build and inspect that starting point
with the separate target.

```console
nix build .#bootstrap-mrustc -L
nu bootstrap/audit.nu .#bootstrap-mrustc --mrustc > /tmp/repkgs-mrustc-inputs.json
```

It is not yet connected to the current Rust and Nushell builds. The remaining work is to build
the intermediate Rust releases, verify each compiler can build the next one and replace the
upstream stage0 components. Our Nushell pin requires at least Rust 1.95. LLVM compatibility
must be checked at each step.

The intended compiler chain is hex0 through the C and C++ bootstrap, mrustc, successive Rust
releases and finally Nushell. The assembled seed will still contain the tools needed for
ordinary package builds. Reducing its compiler trust roots to hex0 does not make that runtime
tool bundle 229 bytes or remove the host and generated-source assumptions above.

## References

- [nixpkgs minimal bootstrap](https://github.com/NixOS/nixpkgs/tree/422d1ae605d7fcd9896412b37dc065c456c0eefb/pkgs/os-specific/linux/minimal-bootstrap)
- [stage0 source preparation](https://github.com/NixOS/nixpkgs/blob/422d1ae605d7fcd9896412b37dc065c456c0eefb/pkgs/os-specific/linux/minimal-bootstrap/stage0-posix/make-bootstrap-sources.nix)
- [live-bootstrap](https://github.com/fosslinux/live-bootstrap)
- [mrustc bootstrap repair](https://github.com/NixOS/nixpkgs/pull/517833)
