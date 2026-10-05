# motley build tools

These workspaces contain the Rust tools used to build motley packages. `bootstrap/` holds the
two crates the toolchain itself is built with, under their own lock so the other tools' dependencies
never move them.

- `mummer` wraps compilers, fetches locked dependencies and relocates outputs.
- `tightrope` supplies the shared daemon protocol types.
- `mummerd` serves the compilation cache and limits concurrent compiler jobs.
- `bigtop` runs package build plans and prepares development trees.
- `jape` bridges Cabal socket tokens to GHC's POSIX semaphore.
- `spieler` updates source pins and dependency locks.

From this directory or `bootstrap/`, build or lint through the repository's Rust shell.

```nu
nix develop .#rust --command cargo build --release --workspace
with-env {RUSTFLAGS: ""} { nix develop .#rust --command cargo clippy --workspace --all-targets }
```

The repository's `docs/mummer.md` describes the cache and build protocols.
