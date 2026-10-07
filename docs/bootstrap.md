# Source bootstrap

The first target is x86-64 Linux. The source peanut build connects LLVM, mrustc, successive Rust
releases and Nushell without downloading a Rust compiler. Its compiler ancestry reaches the
229-byte hex0 executable through nixpkgs' minimal bootstrap.

## LLVM

The pinned nixpkgs already uses its minimal bootstrap for the x86-64 C and C++ toolchain. Its
entry point is the 229-byte AMD64 hex0 executable. The chain builds Mes, TinyCC, successive
GCC versions and the tools needed by the normal nixpkgs build environment.

`pkgs/pe/peanut/build.nix` uses that environment to build our pinned LLVM as a static musl
multicall binary. It includes Clang, LLD and the object tools used by motley. LLVM source
extraction uses the C and shell tools from nixpkgs. CMake, Python and Ninja are selected
without a Rust or Nushell dependency.

```console
nix build .#bootstrap-llvm --no-link --print-out-paths -L
nu bootstrap/audit.nu > /tmp/motley-bootstrap-inputs.json
```

The audit walks build dependencies, including fixed-output fetchers. It rejects Rust, Cargo,
Nushell, mummer and the old binary bootstrap-tools bundle in the LLVM graph, and requires
hex0-seed to be the only executable builtin fetch. Its JSON output records the hashes and
URLs of fixed-output inputs for inspection. Run it after changing the nixpkgs pin or LLVM
build dependencies.

These checks identify dependency mistakes. They do not inspect the contents of source
archives or prove that a compiler is correct. The source hash in `pkgs/ll/llvm/act.lock`
is shared with the normal package set.

## Trust boundary

The host kernel, Nix and the machine running them remain trusted. Nix also needs the pinned
stage0 source tree from its store or a substituter before it can build the first assembler.
That tree contains the bootstrap sources, with peanut executables fetched separately.

Normal builds may substitute compiled dependencies from the configured caches. The recorded
derivations describe their source ancestry, but a cache hit does not verify that ancestry by
rebuilding it. A build from hex0 needs a separate empty store, the pinned stage0 sources and
source downloads, with compiled substitutes disabled.

The nixpkgs chain still accepts generated files in release archives, including some configure
scripts and parsers. Regenerating those from their original inputs is further work before
claiming the stricter live-bootstrap standard.

## Rust and the peanut

`bootstrap-mrustc` builds Rust 1.98.1 and Cargo using our mrustc fork, then rebuilds the compiler
and its standard library with that Rustc. `pkgs/pe/peanut/mrustc.toml` pins the fork revision and
the Rust source release it builds. The shipped Cargo is the one mrustc built.

`bootstrap-rust` uses that compiler to build our pinned Rust source release. It builds stage2
because mrustc's output includes private compiler libraries that conflict with a stage1 Rustdoc
build. The source-built Clang compiles the matching LLVM backend included in the final Rust
release. That build produces a stage2 Rust compiler, Cargo and standard libraries for the GNU
host and the musl target.

`bootstrap-nushell` uses that compiler to build our pinned Nushell source as a static musl
executable. Its features match the normal package recipe. Shell and Python builders handle
the steps before Nu exists. The remaining static userland and C runtime libraries still come
from nixpkgs' source recipes.

```console
nix build .#bootstrap-rust -L
nix build .#bootstrap-nushell -L
nu bootstrap/audit.nu .#bootstrap-nushell --rust > /tmp/motley-bootstrap-inputs.json
nix build .#bootstrap-peanut -L
```

The Rust audit checks the intermediate source URLs and hashes against the manifest and
rejects Rust binary toolchains in the build dependencies. Nix generates the bootstrap config
directly, since its general TOML converter needs Rust. The normal package set still uses
`pkgs/ru/rust-bootstrap/act.lock`. The source peanut build does not depend on that manifest.

Peanut assembly takes Nu from this build and strips its binaries with the source-built LLVM.
It removes store references from the tool binaries and copied data, including Python's build
configuration. Nix rejects the peanut if it retains a dependency on another store path.
These targets do not replace the downloaded peanut used by normal motley builds. Publishing a
new peanut remains separate work.

Use an assembled peanut locally by passing its result link to the package commands. The same
option selects the peanut for `build`, `test`, `repro`, `log`, `shell`, `info` and `list`.

```console
nix build .#bootstrap-peanut --out-link result-bootstrap-peanut --cores 30 --max-jobs 1 -L
nu tools/motley/motley build --peanut ./result-bootstrap-peanut sed diffutils
nu tools/motley/motley log --peanut ./result-bootstrap-peanut sed
```

Select a completed Rust toolchain with `--rust-toolchain`. It supplies Rust and Cargo to
package builds, including tools such as mummerd and formatelf that otherwise use the binary
bootstrap compiler. The option works with the same package commands as `--peanut`, and both
options are accepted by `cache start` when it builds mummerd.

```console
nix build .#bootstrap-rust --out-link result-bootstrap-rust --cores 30 --max-jobs 1 -L
nu tools/motley/motley build --peanut ./result-bootstrap-peanut --rust-toolchain ./result-bootstrap-rust ripgrep
nu tools/motley/motley repro --peanut ./result-bootstrap-peanut --rust-toolchain ./result-bootstrap-rust ripgrep
```

The Nix package-set argument is `rustToolchain`, pointing to an existing store path.
The toolchain must contain `bin/rustc`, `bin/cargo` and the build machine's Rust
standard library. Selection currently supports native builds only. Building the `rust`
package itself still runs its source recipe.

The first package build creates the target libc, runtimes and configured compiler from that
peanut. It reuses the completed Rust bootstrap. Peanut assembly takes its Toybox command links
from the configured target package, so commands enabled there are available when building
the kernel headers and libc.

`bootstrap-mummer` builds the Rust compiler wrapper with the same source compiler and static musl
target. Normal bootstrap recipes copy this binary into the cache wrapper and each configured
toolchain. Its dependencies use the nixpkgs revision pinned in `pkgs/pe/peanut/nixpkgs.json`,
including for non-flake builds and CI. Ordinary flake updates leave the bootstrap compiler
chain alone. Update this pin deliberately when rebuilding that chain. Editing mummer rebuilds
the wrapper without restarting the compiler chain. Peanut assembly includes mummer, while Nushell
is a separate dependency for packages that use Nu phases.

Once assembled, the peanut can be stored and reused for ordinary package builds. Rebuilding
the peanut and checking its inputs remains separate from rebuilding an application.

## Self-hosted build tools

`nix/self-host.nix` builds mummer, bigtop and Nushell through the package set using an existing peanut
and Rust toolchain. `stage1` uses the bootstrap builders. `stage2` rebuilds the three tools
with stage1, and `pkgs` uses stage2 for package builds. Each stage is a separate package set,
so Nushell's build can use an earlier interpreter without depending on itself.

Run these commands in Nushell from the repository root. Each result link protects the
completed outputs from garbage collection. Rerunning a command reuses completed derivations.

```nu
use tools/motley/nix.nu [cache-args]
let bootstrap_args = [--argstr peanut (realpath result-bootstrap-peanut) --argstr rustToolchain (realpath result-bootstrap-rust) --cores 30 --max-jobs 1]
nix-build nix/self-host.nix -A stage1 ...$bootstrap_args ...(cache-args) -o result-self-host-stage1
nix-build nix/self-host.nix -A stage2 ...$bootstrap_args ...(cache-args) -o result-self-host-stage2
nix-build nix/self-host.nix -A pkgs.ripgrep -A pkgs.jq ...$bootstrap_args ...(cache-args) -o result-self-host-packages
```

The provided Rust toolchain supplies Cargo and rustc throughout. This does not rebuild mrustc.
Nushell's system-library dependencies can still require a package-set LLVM build. Its parser
macro patch preserves the deterministic map ordering used by the peanut recipe.

`buildTools = { mummer = ...; bigtop = ...; nu = ...; }` selects tools explicitly when importing
`default.nix` or `nix/set.nix`. Values can be derivations or existing store paths, and omitted
tools keep their bootstrap defaults. The same argument is accepted by `tests/builder` and
`tools/motley/dev.nix`.

The staged mummer handles source unpacking and dependency producers, bigtop runs package builds,
and Nushell runs Nu phases. The compiler wrappers and bootstrap recipes still use the peanut's
static mummer. Replacing that binary requires a static build suitable for copying into each
toolchain. These stages do not remove the initial peanut or establish output reproducibility
without a comparison.

## Building the Rust bootstrap

The pinned nixpkgs provides the C++ compiler and LLVM 22 that build the mrustc fork. Build and
inspect that starting point with the separate target.

```console
nix build .#bootstrap-mrustc -L
nu bootstrap/audit.nu .#bootstrap-mrustc --mrustc > /tmp/motley-mrustc-inputs.json
nix build .#bootstrap-peanut --cores 30 --max-jobs 1 --keep-failed -L
```

Rust's supported bootstrap path uses the preceding minor release or the same one. The compiler
uses unstable features and internals that older compilers cannot reliably build, so moving the
repository's Rust pin past 1.99 needs the fork ported further.

The assembled peanut still contains the tools needed for ordinary package builds. Reducing its
compiler trust roots to hex0 does not make that runtime tool bundle 229 bytes or remove the
host and generated-source assumptions above.

## Resuming and watching the isolated bootstrap

`bootstrap/rebuild.nu` rebuilds hex0 through LLVM in its own store, with compiled substitutes
and remote builders disabled. The default run stops after LLVM. It leaves the completed Rust
chain in the normal store available for ordinary peanut builds.

```console
nu bootstrap/rebuild.nu start
nu bootstrap/rebuild.nu status
nu bootstrap/rebuild.nu log
```

The default is a 30-core budget across three build jobs. Each dependency build gets ten
compile workers, so independent configure and compilation phases can overlap. Compiler
checkpoints get the full budget once their dependencies are ready. `start --cores 22 --jobs 3`
reduces the budget to 22 cores. A recipe may impose a lower limit where parallel compilation
is unsupported.

The runner builds checked LLVM packages separately with four workers by default because
LLVM 20 exhausted the host's memory with ten test workers. `--llvm-cores` changes that limit.
The pinned recipes use `NIX_BUILD_CORES` for both compilation and tests, so this currently
limits both phases. Independent limits need runtime support beyond the existing recipes.
Worker and memory settings stay in the runner and do not change derivation hashes.

All Rust checkpoints use the full compile budget. Final Rust builds its bundled LLVM backend
with two link jobs and does not run LLVM's `check-all` suite.

The service defaults to a 48 GiB memory ceiling, with memory pressure throttling above 80 percent
of that limit. `--memory 64GiB` changes the ceiling. An exhausted limit can still fail a build,
but contains its memory use. `status` and `log` identify kernel OOM kills even when Nix reports
an interruption by the user.

On bcachefs, the checked Coreutils builds use `/tmp` on tmpfs because their hard-link
disk-usage test assumes allocations larger than bcachefs gives small files. Their dependencies
still build on disk, and completed outputs stay in the persistent isolated store.

The build runs as a systemd user service. Closing the terminal or pressing Ctrl-C in `log`
leaves the build running. The log viewer exits when the service finishes.
If the build fails, `log` shows the first Nix error with the preceding build output.

```console
nu bootstrap/rebuild.nu stop
nu bootstrap/rebuild.nu start
```

Starting again reuses every completed derivation. Nix may restart the interrupted derivation
from its beginning. `--keep-failed` preserves its build directory for inspection, but does
not turn it into a compiler checkpoint. Each Rust release in the normal store is already a
separate derivation, so rebuilding the normal peanut does not repeat unchanged compilers.
Keep the `result-bootstrap-rust*` and `result-bootstrap-mrustc` links to protect those outputs
from garbage collection.

The runner saves its evaluated derivation and dependency audit on the first run. Later starts
use that same derivation even after repository edits. State lives under
`$XDG_STATE_HOME/motley/bootstrap-llvm`, or `~/.local/state/motley/bootstrap-llvm` by default.
The directory contains `build.log`, `plan.json`, `graph.json` and the isolated `store`.
`status` counts completed derivations, including source downloads. It is not a time estimate.

Use `--state /path/on/disk` on each command to choose another location or begin a new run
after changing the source pins. Keep it off tmpfs. The first preparation copies only derivation
metadata, declared source inputs and the hash-verified stage0 source tree. The build also
clears inherited sandbox paths so a host `/bin/sh` cannot become an extra compiler peanut.
It first builds Bash 2.05b from hex0 with no `/bin/sh`, then mounts that shell for the
remaining minimal bootstrap. Once static Bash 5.3 is built, it supplies `/bin/sh` for
the LLVM build and its remaining dependencies. Both shells come from the frozen graph
and are built in the isolated store with substitutes disabled.

After a reboot, run `start` again. A full logout may stop the user service if lingering is
disabled. `loginctl enable-linger` allows the user manager to remain active after logout.
The runner does not change that account setting.

When LLVM finishes, the runner compares its NAR hash with the existing LLVM output if one
was available when the run began. A mismatch reports failure and preserves both outputs.

Extend a completed LLVM run through mrustc, each Rust release, Nushell, mummer and peanut assembly
with the same store and log commands.

```console
nu bootstrap/rebuild.nu start --through peanut
nu bootstrap/rebuild.nu status
nu bootstrap/rebuild.nu log
```

The extension requires the full peanut graph to contain the frozen LLVM graph and the same
stage0 sources. It preserves the LLVM plan and records the new plan under `peanut/` in the state
directory. Subsequent `start` commands resume that plan without needing `--through` again.
Each compiler and the assembled peanut have a recorded NAR comparison in `peanut/checkpoints.json`.
If no original output existed when the plan was frozen, its comparison is marked unavailable.
`status` shows these checkpoints alongside the derivation counts.
Completed runs list skipped derivations separately, including source fetchers made unnecessary
by importing verified source inputs.

A NAR mismatch records `different` and allows the remaining checkpoints to build. After the
last checkpoint, any mismatch fails the run with `verification_failed` and lists every
affected checkpoint. Completed outputs remain reusable. This distinguishes a build failure
from a reproducibility failure without treating different outputs as identical.

After fixing a peanut recipe, `start --refresh` evaluates a new peanut plan and archives the
previous plan and comparisons under `history/`. It reuses completed outputs whose derivations
are unchanged and retains the frozen LLVM ancestry. Build the updated normal peanut first to
give the new checkpoints comparison baselines. An ordinary `start` keeps its current plan.

The peanut patches `pest_consume_macros` to generate parser enums in sorted order. Its randomized
map iteration otherwise changes Nushell's generated code between builds. Python's smoke check
disables bytecode writes so it cannot recreate timestamped caches after cleanup. Final Rust
strips unused driver symbols whose ThinLTO suffixes can differ without changing machine code.

The extension reuses LLVM and its dependencies from the isolated store. It also imports locally
available source inputs after checking their pinned hashes and rejecting store references.
Nix's resolved build recipes are checked against the frozen recipes before accepting them as
existing store metadata. Compiled Rust outputs from the normal store stay there, so this step
does compile Rust again to verify its ancestry in isolation. Each completed release is reused
when resuming this run. The normal peanut remains available through `nix build .#bootstrap-peanut`
using the existing Rust outputs.

Archive the isolated store's inputs as a local binary cache. The archive contains the pinned
source files, the auditable hex0 seed and derivation recipes. It follows the active plan, so
a run extended through the assembled peanut includes the Rust and Nushell sources too.

```console
nu tools/archive-bootstrap.nu --destination /path/to/motley-bootstrap-cache
```

The archiver checks each fixed-output input against its pinned flat or NAR hash and rejects
inputs that reference other store paths. It uses 30 hash workers by default, adjustable with
`--jobs`, and compresses the cache with zstd. The manifest records the full source and recipe
inventory. The saved plans include the output hash baselines for the compiler checkpoints.
Rerunning the command reuses archived paths and rejects a destination belonging to another plan.

Copy the archive directory to another x86-64 Linux machine with Nix, Nushell and this repository,
then restore it into a new state directory.

```console
nu tools/archive-bootstrap.nu restore /path/to/motley-bootstrap-cache --state /path/to/offline-bootstrap
nu bootstrap/rebuild.nu status --state /path/to/offline-bootstrap
nu bootstrap/rebuild.nu start --state /path/to/offline-bootstrap
nu bootstrap/rebuild.nu log --state /path/to/offline-bootstrap
```

Restore uses only the local archive. It checks the exact store inventory, saved derivation graph
and every pinned source hash before marking the state ready. An interrupted restore can resume
with the same command. It rejects other nonempty state directories. No compiled LLVM, Rust or
Nushell outputs are imported, and no checkpoints are marked built. Starting the restored state
rebuilds those tools from the saved recipes and compares them with the saved hash baselines.
The existing completed bootstrap remains available in its original state directory.

The verified full-peanut archive contains 354 unique fixed-output inputs and 1,246 recipes,
about 6.7 GiB compressed. Its 1,869 store paths restored in a network namespace with no network
interfaces besides an unconfigured loopback. The 49 early derivations through kaem then rebuilt
offline, and kaem matched the original output's NAR hash. This check did not repeat LLVM or Rust
and does not establish reproducibility across machines.

The host tools remain within the trust boundary described above. Preserving release archives
also preserves their generated files, so this does not close that remaining source audit work.

## References

- [nixpkgs minimal bootstrap](https://github.com/NixOS/nixpkgs/tree/422d1ae605d7fcd9896412b37dc065c456c0eefb/pkgs/os-specific/linux/minimal-bootstrap)
- [stage0 source preparation](https://github.com/NixOS/nixpkgs/blob/422d1ae605d7fcd9896412b37dc065c456c0eefb/pkgs/os-specific/linux/minimal-bootstrap/stage0-posix/make-bootstrap-sources.nix)
- [live-bootstrap](https://github.com/fosslinux/live-bootstrap)
- [mrustc bootstrap repair](https://github.com/NixOS/nixpkgs/pull/517833)
