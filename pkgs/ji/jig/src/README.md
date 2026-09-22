# repkgs build tools

This workspace contains the Rust tools used to build repkgs packages.

- `jig` wraps compilers, fetches locked dependencies and relocates outputs.
- `jigd` serves the compilation cache and limits concurrent compiler jobs.
- `jib` runs package build plans and prepares development trees.
- `jsem` bridges Cabal socket tokens to GHC's POSIX semaphore.
- `uptrack` updates source pins and dependency locks.
- `wire` supplies the shared daemon protocol types.

From this directory, build or lint through the repository's Rust shell.

```nu
nix develop .#rust --command cargo build --release --workspace
with-env {RUSTFLAGS: ""} { nix develop .#rust --command cargo clippy --workspace --all-targets }
```

The repository's `docs/jig.md` describes the cache and build protocols.
