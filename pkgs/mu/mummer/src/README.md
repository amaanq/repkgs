# motley build tools

This workspace contains the Rust tools used to build motley packages.

- `mummer` wraps compilers, fetches locked dependencies and relocates outputs.
- `mummerd` serves the compilation cache and limits concurrent compiler jobs.
- `bigtop` runs package build plans and prepares development trees.
- `jape` bridges Cabal socket tokens to GHC's POSIX semaphore.
- `spieler` updates source pins and dependency locks.
- `tightrope` supplies the shared daemon protocol types.

From this directory, build or lint through the repository's Rust shell.

```nu
nix develop .#rust --command cargo build --release --workspace
with-env {RUSTFLAGS: ""} { nix develop .#rust --command cargo clippy --workspace --all-targets }
```

The repository's `docs/mummer.md` describes the cache and build protocols.
