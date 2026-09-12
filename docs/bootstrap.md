# Source bootstrap

The first target is x86-64 Linux. Build LLVM, Clang and LLD from a small machine-code seed,
then use a pinned upstream Rust toolchain to build Rust, Nushell and Rust jig. Evaluate mrustc
after the LLVM path works.

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

The current seed assembly still uses nixpkgs to build Nushell and the remaining static
userland. The `bootstrap-llvm` target does not yet replace the downloaded seed used by normal
repkgs builds.

The next step is to build Rust against the LLVM toolchain, with an explicitly pinned upstream
Rust binary as the remaining language bootstrap input. That compiler then builds Nushell and
Rust jig. The build steps before Rust must invoke their compilers directly, and cannot use
Nushell recipes or the Rust wrapper.

Once assembled, the seed can be stored and reused for ordinary package builds. Rebuilding
the seed and checking its inputs remains separate from rebuilding an application.

mrustc is a later milestone. The starting reference is nixpkgs' Rust 1.90 bootstrap using
LLVM 20. It needs its own compatible LLVM build and a verified sequence of Rust releases up
to our pin. Replacing the upstream Rust binary closes that remaining compiler bootstrap
dependency.

## References

- [nixpkgs minimal bootstrap](https://github.com/NixOS/nixpkgs/tree/422d1ae605d7fcd9896412b37dc065c456c0eefb/pkgs/os-specific/linux/minimal-bootstrap)
- [stage0 source preparation](https://github.com/NixOS/nixpkgs/blob/422d1ae605d7fcd9896412b37dc065c456c0eefb/pkgs/os-specific/linux/minimal-bootstrap/stage0-posix/make-bootstrap-sources.nix)
- [live-bootstrap](https://github.com/fosslinux/live-bootstrap)
- [mrustc bootstrap repair](https://github.com/NixOS/nixpkgs/pull/517833)
