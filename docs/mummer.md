# mummer, bigtop and mummerd, the contract

What the compiler wrapper, the build executor and their daemon promise to the recipes, the builder and each other.
`design.md` says why they exist. This file says exactly what they do, so either side can be
replaced against it. Byte layouts and key shapes here are normative. Anything not written
here is an implementation choice.

## 1. Binaries and modes

**mummer** is one static executable copied into every toolchain. It picks its mode from `argv[0]`,
or from the first argument when `argv[0]` is `mummer`. `mummer --help` and `mummer <mode> --help` describe
the modes and their flags. The compiler, `rustcwrap`, `slot` and `header-key` modes hand their
arguments through untouched, every other mode parses them and fails on an unknown one.

| invoked as                                                                 | mode                                           |
| -------------------------------------------------------------------------- | ---------------------------------------------- |
| `cc`, `gcc`, `clang`, `cc-build`, `gcc-build`, any other name              | C driver, `cc`                                 |
| `c++`, `g++`, `clang++`, any name containing `++`                          | C++ driver, same code with C++ defaults        |
| `mummer rustcwrap <rustc> <args…>`                                            | rustc wrapper, set as cargo's `RUSTC`          |
| `mummer ghcwrap <ghc> <args…>`                                                | ghc wrapper, set as hadrian's `GHC_WRAPPER`    |
| `mummer cache get\|put <key> <file>`                                          | one object, exit 1 on a miss, 2 with no daemon |
| `mummer cache get-dir\|put-dir <key> <directory>`                             | a directory tree, same exits                   |
| `mummer slot <program> [args…]`                                               | run under a build slot                         |
| `mummer gocacheprog`                                                          | Go's `GOCACHEPROG` protocol on stdin/stdout    |
| `mummer header-key <context> <warning-policy> <compiler> <args…>`             | autoconf header probe key                      |
| `mummer reloc-fixup <directory> [--dest <path>] [--loader <ld.so>] [--deny <path>]…` | post-link relocation                           |
| `mummer nix-store add-text\|add-drv\|fetchurls\|submit …`                     | builder RPC to the Nix daemon                  |
| `mummer bootstrap <recipe>`                                                   | one toolchain stage from its structured attrs, see §13 |
| `mummer fetch <kind>`                                                         | one lock-file producer under builder-rpc-v0, see §14 |
| `mummer assemble`                                                             | lay a producer's collecting derivation out, see §14 |
| `mummer winsdk-assemble`                                                      | the Windows SDK's collecting derivation, see §14 |
| `mummer unpack`                                                               | nix/sources.nix's fetch-and-unpack builder, see §14 |

**bigtop** is the build script executor, a second static executable on every package build's PATH
and nowhere in the bootstrap. It links mummer's library for the store, ELF, tree and cache client
code, so a change to bigtop alone rebuilds packages and not the toolchain. It takes its mode from the
first argument.

| invoked as                                                                      | mode                                                                                |
| ------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------- |
| `bigtop build [--tests --from-tree <tree>]`                                       | the whole package build, the derivation's builder, see §11                          |
| `bigtop shebangs <directory> [--undo]`                                             | point `#!/usr/bin/env` scripts at the build's env, or back                          |
| `bigtop exports <store path…>`, `bigtop deps <store path…>`                           | a package's exports with defaults, or the propagated closure, as JSON               |
| `bigtop write-exports <output>`                                                    | write `exports.json` from the spec on stdin, propagating .pc and cmake requirements |
| `bigtop prune <output>`                                                            | drop docs and junk, unpack man pages, print the inventory as JSON                   |
| `bigtop relativize <prefix> <store path>`                                          | make .pc files, foo-config scripts and links relative, fail on a leaked prefix      |
| `bigtop split-debug <output> <debug output>`                                       | move the DWARF of every ELF mummer linked into the debug output, by build-id           |
| `bigtop env <prefix> [--cache]`                                                    | the build environment from the structured attrs, as JSON                            |
| `bigtop prepare [--from-tree <tree>]`                                              | environment, platform, source tree and ctx before the first build phase             |
| `bigtop finish [--keep-tree] [--tests]`                                            | everything after the last build phase, up to the version check                      |
| `bigtop implant <dir> --interp <ld.so> [--stub <blob>] <dependency…>`              | give upstream ELFs our interpreter, the relocation stub and a padded RUNPATH        |
| `bigtop launchers <output> --launch <bin> --interp <ld.so> [--ldso] <dependency…>` | put launch records in front of bin/ scripts and wrapped binaries                    |
| `bigtop probe key\|restore\|store\|restore-dir\|store-dir\|cmake-init …`           | configure-time probe results shared through mummerd                                   |
| `bigtop system setup\|workdir\|phase <system> …`                                   | every build system                                                                 |
| `bigtop msbuild <project>` | evaluate a project from `{properties, itemdefs}` on stdin |

**mummerd** runs on the host, outside every sandbox, as `mummerd <socket>`. It owns the compile
cache, the file identity cache and the build slots. Nothing in a derivation names it, so
outputs are identical with or without it.

The socket is `MUMMER_SOCK` when set, otherwise `<store parent>/var/nix/mummerd/socket` where the
store parent is `/nix` for `/nix/store`. `tools/motley/motley cache start` maps it into sandboxes with
`extra-sandbox-paths`.

## 2. Configuration

### 2.1 `etc/mummer.json`

mummer resolves `/proc/self/exe`, takes its grandparent as the installation root and reads
`<root>/etc/mummer.json`. Flags and prefix maps are JSON arrays, so spaces inside an argument
stay intact. Without the file mummer passes arguments through unchanged and `MUMMER_CC` must name
the compiler. Fortran invocations require a configured `fc`.

| key          | meaning                                                                            |
| ------------ | ---------------------------------------------------------------------------------- |
| `cc`         | the real compiler, always clang                                                    |
| `fc`         | the Fortran compiler, flang                                                       |
| `binfmt`     | `elf`, `macho` or `coff`, default `elf`                                            |
| `libc`       | libc store path, `<libc>/lib` closes every RUNPATH and holds the interpreter       |
| `interp`     | interpreter file name under `<libc>/lib`, default `ld-linux-x86-64.so.2`           |
| `crt`        | `crt_interp` object, present when executables get the relocatable interpreter stub |
| `flags`      | argument array, first on C and C++ command lines                                  |
| `cxxflags`   | argument array, after `flags` for C++                                             |
| `fflags`     | Fortran arguments, with `@/` paths resolved against the installation root         |
| `prefix-map` | array of `from=to` pairs, each becomes `-ffile-prefix-map=` for C and C++          |

### 2.2 Environment read by mummer

| variable                | meaning                                                                             |
| ----------------------- | ----------------------------------------------------------------------------------- |
| `MUMMER_CC`                | compiler when unconfigured                                                          |
| `MUMMER_SOCK`              | daemon socket path                                                                  |
| `MUMMER_STORE_IDENTITY`    | `content` enables content identity (§3.2), anything else is path identity           |
| `MUMMER_STORE_ROOTS`       | space separated store paths whose masked form may be restored on replay             |
| `MUMMER_STORE_DIR`         | compile-time only, the store prefix, default `/nix/store`                           |
| `out`                   | the derivation's own output path, its hash is masked in keys and objects            |
| `PKGS_CC`               | JSON `{ "<toolchain root>": { cflags, cxxflags, ldflags } }`, the package's flags   |
| `PKGS_PREFIX_MAP`       | colon separated `from=to`, appended to `prefix-map`                                 |
| `MUMMER_SLOT`              | set to a nonempty value by mummer on the compiler it runs, so nested mummers take no slot |
| `MUMMER_LOG`               | append one TSV line per invocation (§10)                                            |
| `MUMMER_LOG_ARGS`          | append the argument line of each uncached invocation                                |
| `MUMMER_NIX_STORE_OFFLINE` | nonempty makes `nix-store` mode compute paths without a daemon                      |
| `NIX_BUILD_TOP`         | the slot owner, `-` when unset                                                      |
| `TMPDIR`                | driver objects directly inside it are excluded from link inputs                    |
| `GOCACHEPROG_DIR`       | where `gocacheprog` materialises outputs, default `$TMPDIR/gocacheprog`             |

### 2.3 mummerd options

`mummerd <socket>` takes its options as flags, each with an environment variable that supplies
the value when the flag is absent. `mummerd --help` lists them.

| flag          | variable         | meaning                                                   |
| ------------- | ---------------- | --------------------------------------------------------- |
|               | `XDG_CACHE_HOME` | packs live in `<it>/mummerd/packs`, default `$HOME/.cache`   |
| `--size`      | `MUMMERD_SIZE`      | tier 0 budget in GiB, default 100, 0 means unlimited      |
| `--cold`      | `MUMMERD_COLD`      | a second tier directory that takes what tier 0 pushes out |
| `--cold-size` | `MUMMERD_COLD_SIZE` | its budget in GiB, default unlimited                      |
| `--slots`     | `MUMMERD_SLOTS`     | concurrent compiler slots, default the CPU count          |

## 3. Store paths, masking and identity

`STORE_DIR` is `/nix/store` unless built with `MUMMER_STORE_DIR`. A **store path** is any string
starting with `STORE_DIR/`.

### 3.1 Hash masking

`mask(text)` replaces every `STORE_DIR/<32 alphanumerics>-` with `STORE_DIR/*-`. Only the 32
characters directly after `STORE_DIR/` are considered, and only when a `-` follows them.
Masking is applied to keys, to stored text objects and to file listings so that a rebuilt
dependency with new store hashes and identical bytes still hits.

### 3.2 Identity modes

Path identity, the default, treats a store path as immutable and identifies it by name. The
input identity of a store file is `S:<path>`, and nothing is masked.

Content identity, `MUMMER_STORE_IDENTITY=content`, is what the builder sets. Store hashes are masked
everywhere, every input is identified by content, and `MUMMER_STORE_ROOTS` tells mummer which real
paths a masked root may stand for.

### 3.3 Roots

A root is `STORE_DIR/<hash>-<name>` cut at the first `/` after the name. mummer collects roots from
`MUMMER_STORE_ROOTS` and `$out`, keyed by their masked spelling. The first real path wins per
masked root. `resolve(masked)` maps `STORE_DIR/*-<name>/<rest>` back to the real root plus
`<rest>`, and fails when no root matches. `resolve_all(text)` masks the text and then replaces
each known masked root wherever it is followed by `/`, by one of `\t\n\:"';,)]}>`, or by the end
of the text.

### 3.4 Output placeholder

In content mode, when `$out` is a store path, its 32 character hash is replaced by 32 `e`
characters in keys, in cached objects and in cached diagnostics, and restored on replay. The
replacement requires the store directory immediately before the hash and a dash immediately
after it. It applies to binary objects as well as text, leaving unrelated runs of `e` untouched.

### 3.5 Input identity

`input_id(path)` is `S:<path>` for a store path in path mode. Otherwise it is
`C:<first 32 hex of blake3(bytes)>` where bytes are the file contents with the output placeholder
applied when the path is not a store path. mummer asks the daemon for store file identities in
batches (§6.6) and computes the rest itself.

`tool_id(program)` locates the program on `PATH`, canonicalises its directory and keeps its
file name. A store path compiler yields `key(filename)` (§4), any other yields
`<filename>=<input_id>`.

## 4. Argument key normalisation

`key(arg)` is the normal form of an argument for hashing. It is `mask_for_replay(names(paths(arg)))`
where

- `paths` rewrites the path value of an option through `normalize` (drop `.`, fold `..` except
  after a leading `..` or at the root, empty becomes `.`). The value starts after `--opt=`, after
  `-o=` when the option letters contain no `/`, otherwise after the two character option. `-D` and
  `-U` are never touched, a bare `-x` is never touched, and only values containing `/` or starting
  with `.` are rewritten.
- `names` replaces the 6 alphanumerics after `TryCompile-` and the 5 after `cmTC_` with `#`,
  so cmake's per-run scratch names do not fork the key.
- `mask_for_replay` is `mask` in content mode and the identity otherwise.

## 5. Requests, manifests and results

A cache entry is a **request** (everything known before the compiler runs), a **manifest** of
what the compiler actually read, and the **result** objects. The request key selects the
manifest, the manifest validates against the current tree and yields the result key.

Every hash below is blake3, hex, truncated to 32 characters. `field(h, bytes)` feeds
`<decimal length>:` then the bytes, so fields cannot run into each other.

### 5.1 Manifest

```json
{ "inputs": [ { "path": "<key(path)>", "identity": "<input_id>" }, … ],
  "environment": [ { "name": "…", "value": "…" | null }, … ] }
```

Inputs are sorted and unique. Lookup resolves each masked path (§3.3), recomputes its identity
and compares, and compares each environment value with `getenv`. A missing input or any mismatch
is a miss. The result key is `hash(field(request) field(manifest JSON))`.

Manifests are stored at `m/<request>`. Result objects live under `<letter>/<result>`.

### 5.2 C and C++ requests

The invocation is parsed after response file expansion. Flags that end up in `key_args` are
every argument except `-o <x>`, `-o<x>`, `-MF <x>`, `-MT <x>`, `-MQ <x>`, `-MD`, `-MMD`,
`-MP`, `-Wp,-MD,<x>`, `-Wp,-MMD,<x>` and the source file. Arguments mummer injects from
`mummer.json` and `PKGS_CC` that the user did not pass are appended to `key_args`.

```
request = "cc/" + hash(
   field(tool_id(cc))
   field(key(cwd))
   field("link" | "link-one" | "precompile" | "compile")
   for each key_arg a: field(links && (a contains "dynamic-linker" || a contains "rpath")
                              ? placeholder(a) : key(placeholder(a)))
   for each of CPATH C_INCLUDE_PATH CPLUS_INCLUDE_PATH OBJC_INCLUDE_PATH LIBRARY_PATH
               SOURCE_DATE_EPOCH LANG LC_ALL: field(name) field(value or "")
   field(exact source argument) field(placeholder(source bytes))   unless the action is a pure link
   for each object, archive or library input: field(input_id(input)))
```

The source argument preserves its spelling because `file.c` and `./file.c` produce different
`__FILE__` values.

`placeholder` is the §3.4 substitution. An invocation is cacheable when it has exactly one
source for compile, assemble or preprocess, or is a link of objects, is not a query
(`-M`, `-MM`, `-E` without a source, `-v`, `--version`, `-###`, `-fsyntax-only`, `-print*`,
`--print*`, `-dump*`), does not write `/dev/null`, `.gch`, or a `.pch` outside a precompile, and
carries none of the markers `@file`, `,@`, `-Map`, `--print-map`, `--dependency-file`, `--out-implib`,
`--trace`, `--verbose`, `--print-gc-sections`, `--stats`, `-save-temps`, `-ftime-trace`,
`-fprofile`, `--coverage`, `-fmodules`, `-MJ`, `-serialize-diagnostics`, `-gsplit-dwarf`,
`,-M`, `,-t`. A `-header` language is cacheable only as a precompile. Links are cacheable for ELF only.

A precompile is clang's `-Xclang -emit-pch` with a `-header` language and a `.pch` output, as
CMake writes it. mummer adds `-Xclang -fno-pch-timestamp`, cached or not, since clang would
otherwise refuse a restored `.pch` whose headers carry other mtimes, and keys it with exact store
paths (§3.2 path identity whatever the mode), since a `.pch` names the absolute headers clang
reopens. A compile reading `-include-pch <file>` takes the `.pch` as an object-style input, its
bytes in the request key and its identity in the manifest. Its depfile lists the headers inside
the `.pch` but not the file itself.

The compiler, explicitly selected linker, and their sibling LLVM, clang-cpp and lld libraries
also contribute their identities to the request key. Patching a tool at the same version must miss.

The manifest inputs are the depfile entries, including system headers, lld's
`--dependency-file` entries, the source and the object inputs. Driver objects directly inside
`TMPDIR` are excluded, while inputs in its subdirectories remain. The manifest environment
is empty. A failing compile is stored only when it had inputs, did not fail with `file not found`,
and neither exited from a signal nor reported a crashed child. A failing link is never stored.

| object       | content                                                                                       |
| ------------ | --------------------------------------------------------------------------------------------- |
| `r/<result>` | exit status in decimal                                                                        |
| `o/<result>` | the output with the placeholder applied, absent on failure                                    |
| `d/<result>` | the depfile text with `mask_for_replay` and the placeholder, only when the user asked for one |
| `e/<result>` | stderr with the placeholder applied                                                           |

Replay restores the placeholder, writes the object (mode 755 for links), rewrites the depfile
through `resolve_all` and retargets the text before the first `:` to `-MT`/`-MQ` or the output,
and replays stderr. `-E` to stdout is cached as an object and printed.

### 5.3 rustc requests

Parsed from the arguments after the compiler. `key_args` holds each named flag as `flag=value`,
except `--out-dir`, `-L` and `-C extra-filename=…`, plus every other argument. `-C metadata`
stays in the key because it seeds the crate's symbol hashes.
`-Cx` glued spellings become `-C=x`. Cacheable when there is one `.rs` source, `--emit` includes
`dep-info`, `--crate-type` is only `lib`, `rlib` or `staticlib`, `--out-dir` and `--crate-name`
are set, no `-C incremental`, no `-o`, no `@file` and no query (`--print`, `-vV`, `--version`, `-`).

```
request = "rs/" + hash(
   field(tool_id(rustc))
   field(key(cwd))
   for each key_arg a: field(key(a))
   field("source") field(exact source argument)
   field(source bytes)
   for each --extern path: field(input_id(path))
   for each libstd-*.rlib and libcore-*.rlib under <rustc root>/lib/rustlib/*/lib, sorted:
      field(input_id(rlib))
   for each of CARGO_PKG_NAME CARGO_PKG_VERSION CARGO_CFG_TARGET_FEATURE RUSTFLAGS
               CARGO_ENCODED_RUSTFLAGS LANG LC_ALL: field(name) field(value or ""))
```

The manifest inputs are the dep-info entries, the externs and the source. The manifest
environment is every `# env-dep:NAME` line of the dep-info. Every target named before `:` in
the dep-info plus the dep-info itself are the artifacts.

| object                | content                                                                            |
| --------------------- | ---------------------------------------------------------------------------------- |
| `b/<blake3 of bytes>` | one artifact, content addressed                                                    |
| `o/<result>`          | JSON `[ { "name", "mode", "hash" } ]`, names with `extra-filename` replaced by `@` |
| `e/<result>`          | stderr                                                                             |

Replay writes each artifact to `<out-dir>/<name with @ replaced by this extra-filename>` after
checking the content hash and refusing names with a directory component.

### 5.4 Header probe keys

`mummer header-key` serves the `CONFIG_SITE` the autotools system writes, which patches autoconf's
C compile test to ask mummer before compiling. It only answers for `cc`, `gcc`, `cc-build` and
`gcc-build` that resolve to the running mummer, a store path compiler, a last argument of
`conftest.c`, arguments from a
fixed allow list, and none of `CCC_OVERRIDE_OPTIONS`, `CL`, `_CL_`, `GCC_EXEC_PREFIX`,
`COMPILER_PATH`, `SDKROOT` set.

First it looks the full compile up as a §5.2 request. A manifest hit whose result has status
`0` and empty diagnostics answers exit 0 without preprocessing. Otherwise it preprocesses with
`-E -MD -MT probe -Wunused-macros -fno-color-diagnostics -fno-caret-diagnostics` under
`LC_ALL=C`. `PACKAGE_*` and `VERSION` defines that clang reports unused are blanked in the source
text, any other diagnostic disqualifies the probe.

```
key = "probe/header/" + full hex of hash(
   field(context) field(compiler) field(warning policy)
   for each expanded argument: field(arg)
   for each of CPATH C_INCLUDE_PATH CPLUS_INCLUDE_PATH OBJC_INCLUDE_PATH LIBRARY_PATH
               SOURCE_DATE_EPOCH LANG LC_ALL LC_CTYPE LC_MESSAGES: field(name) field(JSON of value)
   field(normalised source) field(preprocessed output)
   for each depfile entry except the source, canonical path: field(path) then field(bytes)
      unless the path is a store path)
```

Exit 0 when the daemon holds `yes\n` at that key, exit 3 with the key on stdout when it holds
nothing, exit 4 when the preprocessor exits 1, exit 1 for every other unsupported case. The
autoconf side stores `yes\n` after a successful compile and answers exit 4 as a failed check
without compiling, since the compile runs the same preprocessor with the same flags. Only
successful, warning free header checks are shared.

### 5.5 Go

`mummer gocacheprog` speaks Go's cache program protocol. Action and output ids are 32 bytes, base64
on the wire, hex in keys. `go/a/<action hex>` holds the 32 output id bytes, `go/o/<output hex>`
holds the output. Bodies are materialised under `GOCACHEPROG_DIR`. Without a daemon every get
is a miss and every put is a local write.

### 5.6 GHC modules

`mummer ghcwrap` serves hadrian's per module `ghc -c`, which a local hadrian patch runs under
`$GHC_WRAPPER`. Cacheable when there is `-c`, one `.hs`, `.lhs`, `.hs-boot` or `.lhs-boot`
source, a relative `-o` ending in the object suffix, and none of `-dyno`, `-dynohi`, `-hidir`,
`-odir`, `-hiedir`, `-stubdir`, `-dumpdir`, `-fwrite-ide-info` or `-ddump*`.

```
request = "hs-module/" + hash(
   field(input_id(canonical ghc)) field(key(ghc --info))
   field(key(cwd))
   for each argument a: field(key(a))
   for each of GHC_PACKAGE_PATH GHC_ENVIRONMENT SOURCE_DATE_EPOCH LANG LC_ALL:
      field(name) field(value or "")
   field("source") field(source bytes))
```

The manifest inputs come from the nearest `.dependencies` above the object. They are its line,
then the lines of every object whose interface it lists, transitively, plus the source and any
file named by an argument (`-optP…`, `-optc…`, `-ghcversion-file=…`). Interfaces of other
packages are taken as they are, a change below them shows in their own bytes. Files a Template
Haskell splice reads are not tracked.

The artifacts are the object, its interface, their `dyn_` pair under `-dynamic-too`, and the
`_stub.h` when one was written. `o/<result>` holds `[ { "path", "mode", "hash" } ]` with paths
relative to the working directory, `b/` and `e/` are as in §5.3.

### 5.7 Directories

`put-dir` uploads a tree as content addressed blobs plus one listing under the caller's key.
Each regular file is stored with `mask` applied when it is NUL free UTF-8, so the blob hash is of
the masked bytes. The listing is one line per entry in directory order, sorted by name, parents
before children.

```
f <mode octal> <blob hash>\t<relative path>
l 0 <target>\t<relative path>
d 0 -\t<relative path>
```

`get-dir` rejects a listing with a duplicate path, a path with a non normal component, a mode
above `0777`, a file beneath a file or symlink, or a blob whose hash differs. It builds the tree
in a staging directory beside the destination, restores text blobs through `resolve_all`, then
renames over the destination. `builder/probe-cache.nu` keys these trees itself.

## 6. Wire protocol

One Unix socket connection per mummer process, request lines terminated by `\n`, at most 65536
bytes per line. A client may pipeline requests. The server answers in order.

| request                               | reply                                           |
| ------------------------------------- | ----------------------------------------------- |
| `GET <key>`                           | `MISS\n`, or `OK <n>\n` then `n` bytes of frame |
| `HAS <key>`                           | `1\n` or `0\n`                                  |
| `PUT <key> <n>` then `n` frame bytes  | `OK\n`                                          |
| `IDS <count>` then `count` path lines | `count` lines, each `C:<32 hex>` or empty       |
| `SLOT <build>`                        | `OK\n` once a slot is granted                   |
| `DONE <build>`                        | `OK\n`                                          |
| `STATS`                               | one line of `name=value` pairs                  |

A key is 1 to 4096 bytes with no whitespace and no NUL. The daemon stores frames as opaque
bytes and never inspects them.

On Linux, connections are accepted from the daemon's user, root, the `nixbld` group and
Nix's automatically allocated build UIDs. Request lines are limited to 64 KiB.

### 6.1 Frame

```
u32 native endian   uncompressed length
zstd                the object, level 1, single frame
```

Both the object and the frame are limited to 2 GiB. A reader rejects a frame shorter than 4
bytes, a decompressed size that differs from the header, and trailing bytes.

### 6.2 Identities

`IDS` is only asked for store paths that are not under `$out`. The daemon restricts requests
to `NIX_STORE_DIR`, defaulting to `/nix/store`, checks symlink targets and on Linux checks the
opened descriptor too. It answers with the `C:` identity of a regular file, cached by device,
inode, size, mtime and ctime, and an empty line for any rejected path or failed read. A client
that gets an empty line hashes the file itself.

### 6.3 Slots

A slot is admission to run one compiler. mummer takes one around every compiler it runs unless
`MUMMER_SLOT` is set, and sets `MUMMER_SLOT=1` on the child so nested wrappers do not. `mummer slot`
does the same for arbitrary programs. `<build>` is `NIX_BUILD_TOP`, so the daemon can favour the
build holding the fewest slots when several are waiting. Ties go to the earliest request. A
client that disconnects while waiting is removed from the queue, and every slot a connection
holds is released when it closes.

`jape <ghc> [args…]` bridges Cabal 3.18's socket tokens to GHC 9.14's POSIX semaphore.
Its `--info` reports semaphore protocol 2 so Cabal enables `-jsem v2-<name>`. The wrapper
gives GHC a private named semaphore and acquires a mummerd slot before starting it. Cabal already
owns GHC's implicit first token. Each extra capability needs both a Cabal token and a mummerd slot,
and the bridge posts to the semaphore only after both grants arrive.
The bridge keeps one backed token idle because POSIX semaphores cannot report waiting clients.

Returned tokens close both backing connections. Child exit refunds every grant and unlinks
the semaphore. A bridge error kills the child process group and reaps GHC before refunding
grants. The child inherits `MUMMER_SLOT=1` so its cc and ld processes use their parent capability's
slot. If mummerd is unavailable the bridge fails before starting GHC.
Invocations without a v2 semaphore run GHC directly.

## 7. mummerd storage

### 7.1 Packs

Objects are appended to pack files. Each tier directory holds `<id:06>.pack` files and their
`<id:06>.hint` indexes, and is `flock`ed exclusively for the daemon's lifetime.

```
record   u32 le key length | u32 le value length | key bytes | value bytes
hint     u32 le key length | u32 le value length | u64 le record offset | key bytes
```

A new record goes to the active pack in tier 0. The active pack rotates once appending would take
it past 256 MiB. Sealing a pack writes its hint atomically. On open, a pack with a hint is
indexed from it, a pack without one is scanned and truncated at the first record that does not
fit, then sealed. A pack that fails to load is skipped with a warning, never fatal. Pack ids
are never reused within a store, and a pack found in a later tier with an id already loaded is
deleted as a leftover of an interrupted move.

The in-memory index maps each key to its newest location. Live bytes per pack are the bytes of
records that are still current. The highest pack ID wins across tiers. Hint entries extending
past the pack's end are skipped. Reads use `sendfile` and hold the pack open through eviction.

PUT bodies of at least 4 MiB are spooled into the hot tier before taking the store lock.
Smaller bodies are buffered in memory. An incomplete transfer is never indexed.

### 7.2 Tiers and eviction

Tier 0 is `XDG_CACHE_HOME/mummerd/packs` with the `MUMMERD_SIZE` budget, tier 1 is `MUMMERD_COLD` with
`MUMMERD_COLD_SIZE`. A tier over budget demotes packs, never the active one, choosing first any
pack with less than half its bytes live, then the least recently read, then the lowest id.
Demotion from the last tier deletes the pack. Otherwise the pack and hint are hard linked into
the next tier, or copied and renamed when linking fails, and removed from the old tier.

A pack's recency is its file mtime. A read touches the mtime at most once a minute, and a
demoted pack keeps its mtime, so recency survives restarts and moves. The demoter runs on its
own thread, woken by any put that takes tier 0 over budget, and is joined before the daemon
exits.

## 8. Driver argument policy

Given the user's arguments, mummer builds the compiler command line in this order.

1. `--start-no-unused-arguments`
2. `flags` from `mummer.json`
3. `PKGS_CC.cflags` for this toolchain root, minus any `_FORTIFY_SOURCE` define when the user
   set one or compiles with `-O0`
4. for C++, `--driver-mode=g++`, `cxxflags`, `PKGS_CC.cxxflags`
5. `-ffile-prefix-map=<pair>` for each `prefix-map` and `PKGS_PREFIX_MAP` pair
6. for an ELF link with an input, `-Wl,--build-id=sha1` and `-Wl,--package-metadata={"type":"motley"}`
7. `--end-no-unused-arguments`
8. the user's arguments after the per format filter, with every rpath spelling removed for ELF
9. when linking with an input, `PKGS_CC.ldflags`
10. the link policy below, unless the user passed `-static`, `-static-pie`, `-r`, `-Wl,-r` or
    `-nostartfiles`

The per format filter drops `-Wl,--dynamic-linker*` and `-Wl,-dynamic-linker*` for ELF, drops
`-fPIC` `-fpic` `-fPIE` `-fpie` `-fno-PIC` `-fno-pic` `-pie` and raises `-std=c++98|03|0x|11`
and the `gnu++` spellings to 14 for COFF, and keeps everything for Mach-O.

**ELF link policy.** The linker gets `-Wl,--undefined-version`, `-Wl,-rpath,/` followed by 4095
underscores, and `-Wl,--enable-new-dtags`, and the user's own rpaths go through as given. After a
link that succeeds, mummer reads the linker's dependency file, the one the arguments name or
`-Wl,--dependency-file=<scratch>` it adds, and rewrites the output's RUNPATH in place: the user's
entries not under `/usr`, `/lib` or `/opt`, then `<dir>/.:` for each store directory holding a
shared library the linker opened, then `/` and underscores to the end, the slack `reloc-fixup` rewrites into. When `48 × max(entries, 1) + 80 × needed` does not
fit, the link runs again with that much room rounded up to a power of two, so an output's bytes
depend on what it links and never on how mummer reads its arguments. Executables (not `-shared`)
get `-Wl,--dynamic-linker=<libc>/lib/<interp>`, or with `crt` set, `-x none <crt>`, the same
interpreter spelled with twelve `./` segments so the stub can be written over it, and
`-Wl,--export-dynamic-symbol=__reloc_start`.

**Mach-O** links get `-Wl,-headerpad_max_install_names`. **COFF** links get nothing.

## 9. reloc-fixup

`mummer reloc-fixup <prefix>` walks every regular file under the prefix, skipping `.debug` files.
`--dest <store path>` names where the tree will finally live when it differs from the prefix on
disk, and defaults to the prefix, which must itself be a store path. `--deny <store path>` fails
any file whose bytes contain that path's hash. Every failure is reported and counted, and the
run exits nonzero after the walk if any file failed.

Library directories are every directory under the prefix, outside `/debug`, that holds a file
whose name contains `.so` or ends in `.dylib`.

### 9.1 ELF

For executables and shared objects with a `DT_RUNPATH` or `DT_RPATH`, each existing entry is
mapped to its final location (`$ORIGIN` entries relative to the file's final directory, others
through the prefix to dest mapping), kept when it is a store path and spelled `$ORIGIN/<relative>`.
The library directories of the output are appended as candidates. Every `DT_NEEDED` without a
`/`, other than the `--loader` itself, is searched in that order, failing the file when none has
it. A library found outside the loader's directory, the libc's, is rewritten to
`$ORIGIN/<relative>/<name>` directly and its directory leaves the RUNPATH. Without `--loader`
every library found is rewritten. bigtop passes the platform's interpreter. The RUNPATH string, then the direct NEEDED strings NUL separated,
must fit the old RUNPATH string, otherwise every directory stays in the RUNPATH and NEEDED is
left alone. `.gnu.version_r` entries naming a rewritten library are repointed. A symbol name
sharing bytes with the RUNPATH string is an error.

When the file exports `__reloc_start`, or the sixteen bytes before the entry point read
`RELOCSTB`, `PT_INTERP` is rewritten to the interpreter's path relative to the file's final
directory, the segment type is cleared to `PT_NULL`, and the entry point is set to the stub.

Each rewritten file prints `relocated <path relative to prefix>`. A file that still contains
`STORE_DIR/` prints `absolute store reference remains in <path>`.

### 9.2 Mach-O

For 64 bit images, `LC_LOAD_DYLIB`, `LC_LOAD_WEAK_DYLIB`, `LC_REEXPORT_DYLIB`,
`LC_LOAD_UPWARD_DYLIB` and `LC_RPATH` paths that map to a store path become
`@loader_path/<relative>`, `LC_ID_DYLIB` becomes `@rpath/<file name>`. Paths starting with `@`,
`/usr/lib/` or `/System/` are kept, and a load path outside the store fails the file. Rewritten
commands are re-padded to 8 bytes, the load command area is rebuilt in place and must fit the
room before the first section or segment content, otherwise the file fails with the space it
needed. An `LC_CODE_SIGNATURE` with a SHA-256 code directory is re-signed page by page. The file
prints `relocated <path>  <new spellings>`.

## 10. Invocation log

Each mummer invocation appends `tool\toutcome\tsubject\tmilliseconds` to `MUMMER_LOG`. `subject` is
the source, output or crate label with tabs and newlines replaced by spaces. After a cache
lookup that did not replay, the subject carries the reason after a space, so a miss can be
told apart from a first compile.

| reason                  | meaning                                                        |
| ----------------------- | -------------------------------------------------------------- |
| `new-key`               | no manifest for the request                                    |
| `inputs-changed:<path>` | that input's identity differs from the manifest, or it is gone |
| `env-changed:<name>`    | that manifest variable differs                                 |
| `object-gone`           | the manifest matched but a result object was evicted           |
| `replay-failed`         | the daemon or the replay failed, see stderr                    |

| tool     | outcomes                                                                                                                             |
| -------- | ------------------------------------------------------------------------------------------------------------------------------------ |
| `cc`     | `cached`, `cached-error`, `compiled`, `compiled-error`, `compiled-unstored`, `failed`, `linked`, `uncacheable`, `no-daemon`, `query` |
| `rustc`  | `cached`, `compiled`, `compiled-unstored`, `failed`, `uncacheable`, `no-daemon`, `query`                                             |
| `header` | `compile-hit`, `hit`, `miss`, `unsupported-compiler`, `unsupported-options`, `preprocess-failed`, `diagnostics`                      |
| `go`     | `cached`, `compiled`, `compiled-unstored`                                                                                            |

`bigtop finish` tallies the file per package and treats `cached`, `cached-error`, `hit` and
`compile-hit` as hits. `MUMMER_LOG_ARGS` gets `outcome\t<arguments joined by spaces>` for every
non query invocation that ran the compiler.

## 11. Builder modes

`bigtop build` runs these in order, prepare, each build system's setup, the phases, finish. `bigtop prepare` and `bigtop finish` stay callable on their own.
Paths and errors follow the nu functions they replaced.

`bigtop build` emits `@nix` JSON with `action = "setPhase"` before preparation, each build
system's setup, each selected Rust or Nu phase, and finish. Setup names are `<system>.setup`,
and package phases use their plan names. Nix attaches these updates to the build activity
for ROM to display. Nu phase bodies do not repeat the dispatcher's announcement.

`prepare` computes the `env` environment, resolves the platform (a cross build probes
`platform.probe --version` for transparent binfmt and drops the emulator when it has it), copies
and patches the source into `source/` or restores a tests derivation's tree, and writes the
`ctx` record to `$NIX_BUILD_TOP/ctx.json`. It prints `{ env, cwd, ctx }` on stdout, tool output
goes to stderr.

Aarch64 cross builds reserve 16 TiB of guest virtual address space through `QEMU_RESERVED_VA`.
The binfmt probe uses the same reservation as the build. If the probe starts but exits
unsuccessfully, prepare fails with its stderr so emulator startup errors remain visible.

`finish` reads `ctx.json` and runs the finish sequence in the directory the last phase left.
That is `spec.install` and `spec.links`, `--keep-tree`, the `bin` check, `prune`, the layout check,
`shebangs --undo`, `split-debug`, `implant`, `launchers`, `reloc-fixup`, `exports.json`,
`relativize`, the move into the store, the `tests.version` check from the store path and from a
relocated copy under `env -i`, and the `mummer:` cache summary. `--tests` writes the tests
derivation's `result.json` instead.

`probe key <kind> <scripts…> [--extra <json>]` prints `probe/<kind>/<hash>` over the scripts'
contents, `extra`, the full store roots, mummer's injected flags (`PKGS_CC`) and the cc toolchain's
content. Store hashes remain in the key because configure results can depend on absolute paths.
`$out` stays out, since a CA derivation's is a placeholder hashed from every input. `store` drops
lines that name `$out` instead, and `store-dir` refuses a tree that names it. `restore` and `store` move file bytes through
mummerd, with failed uploads reported without failing the build. `restore-dir` and `store-dir` move a tree,
`cmake-init` renders a `CMakeCache.txt`'s path-free `INTERNAL` entries as a `-C` script.

`system setup|workdir|phase <name> [<verb>]` is every build system, package.nix calls them
from the build script. autotools keeps `config.cache` and cmake its `INTERNAL` entries under the probe key of
the same name. cargo and python read
the `[bindings]` of the build's dependencies, the locked C bindings each library links and the
environment they get, as the `sysLibs` attr.

`env` reads the derivation's structured attrs from `NIX_ATTRS_JSON_FILE` and prints the
environment every build step inherits, one JSON object for `load-env`, with `PATH` as a list.
Later entries override earlier ones, in this order. Sandbox dirs (PATH over every build
dependency's `bin/`, HOME and the XDG dirs under `$NIX_BUILD_TOP/home`, which it creates,
TMPDIR, `CI=true`, `TERM=dumb`, `out` set to the prefix argument). Reproducibility pins
(`SOURCE_DATE_EPOCH=315532800` and the matching KBUILD, TZ, LC_ALL, ZERO_AR_DATE and hash seed
variables, `CONFIG_SITE` from the attrs). The toolchain's view of the dependency closure (CC and
friends by bare name, PKG_CONFIG_PATH, CMAKE_PREFIX_PATH and ACLOCAL_PATH from the closure's
`exports`, MUMMER_LOG and MUMMER_LOG_ARGS under the build top, `MUMMER_STORE_IDENTITY=content`,
`MUMMER_STORE_ROOTS` over the closure, the build dependencies, each build system's lock tree
(`spec.<uses>.deps`, the vendored crates, modules or wheels every locked compile reads) and
what the `cc` on the new PATH lists in its package's `etc/roots`, and `PKGS_PREFIX_MAP` mapping
the build top to `/build` and each store path to `/deps/<name>` or `/tools/<name>`). `PKGS_CC`
(§ 2.2) keyed by the resolved root of that `cc`, with `-isystem` over the closure's include
dirs, `-O2`, `-g` or `-g0` after `spec.debug`, frame pointers, the hardening flags
selected by bigtop's hardening table, then `platform.hardening` and `spec.cc.hardening` in that
order (`false` turns all off), the package's own `cc.cflags`, and the same for `cxxflags` and
`ldflags` with `-L` over the closure's lib dirs, plus the cargo release profile's debug and
strip settings. Then every build dependency's `exports.env`, then `spec.env`. With `--cache`,
when `rustc` is on the new PATH, `RUSTC` absolute, `RUSTC_WRAPPER` pointing at `rustcwrap` and
`CARGO_INCREMENTAL=0`, and `GOCACHEPROG` when `go` is.

`bigtop schema` emits the hardening vocabulary from the Rust enum. Nix reads the committed
`nix/schema.json` to reject unknown recipe keys during evaluation. After changing the vocabulary,
regenerate it with `bigtop schema | save --force nix/schema.json` in Nushell. CI runs
`bigtop schema --check nix/schema.json` to catch drift. Boolean settings and per-flag boolean maps
are accepted, other values fail instead of silently disabling flags.

`shebangs` rewrites the first line of every executable regular file under 1 MiB that starts
with `#!/usr/bin/env` to `#!<env on PATH>`, writing in place and restoring the file's mtime.
`--undo` looks at every regular file, replaces that spelling on any line with `#!/usr/bin/env`,
and leaves mtimes alone.

`exports` prints one record per store path with the `exports.json` fields filled from the tree
when absent (§ `builder/exports.nu` documents the defaults), `env` values with `{root}` expanded,
and `root` added. `deps` prints the breadth first closure over `propagate`, each root once, in
discovery order. `write-exports` reads `{ name, ext, exports, deps }`, where `ext` carries the
platform's executable, shared and import library suffixes, and `exports` is the spec's
record, `false` for nothing to link against, or null, and writes the merged record with the
dependencies named by `Requires` lines in the output's `.pc` files and `find_dependency` calls
in its cmake configs appended to `propagate`.

`prune` removes `share/doc`, `share/info` and `share/gtk-doc`, `.la` files, `perllocal.pod`,
`.packlist` and `lib/charset.alias`, gunzips `share/man/**/*.gz` and fails on a `.pch` or
`.gch`. It prints the remaining entries as `[{ type, size, path, target, rel }]` sorted by
`rel`, with `type` one of `f`, `l`, `d`.

`relativize` rewrites the prefix in `.pc` files to `${pcfiledir}/..` chains, turns
`prefix=<prefix>` assignments in `bin/` shell scripts into a `$0` relative form, rewrites
absolute symlinks to relative ones as they will sit under the store path, refuses a link that
leaves the store or dangles, and finally fails if any regular file still contains the prefix,
naming up to ten files and the printable runs around the hit.

`split-debug` runs `llvm-objcopy --strip-debug` over every `.a` and `.o` in the output, noting
the ones it refuses (archives with LTO bitcode or `lib.rmeta` members keep their DWARF). It then
reads every other regular file over 3072 bytes that is a 64-bit ELF and classes it by its notes
and section names. An ELF is _ours_ when it carries the FDO package note (type `0xcafe1a7e`)
the driver's `--package-metadata` produces (§ 8), _split_ when it has a `.gnu_debuglink`, and it
has DWARF when a `.debug_info` section is present. Ours with a nonempty `.text` section but
without DWARF or a debuglink is an error, since the build stripped what the driver compiled
with `-g`. Stubs with no code or DWARF stay unchanged. Anything not ours or
already split is left alone and counted in a `debug:` note. The rest is grouped by GNU build-id
(ours without one is an error), each group's first file copied with `--only-keep-debug` to
`<debug>/lib/debug/.build-id/<id[..2]>/<id[2..]>.debug` and every member stripped with
`--strip-debug --add-gnu-debuglink=<that file>`, groups in parallel. A final `debug:` note gives
the number of build-ids and the apparent size of the debug output.

`implant` serves `prebuilt = true` packages. Every regular 64-bit ELF under `bin/`, `lib/` and
`libexec/` of the directory that has a `PT_DYNAMIC` segment gets `formatelf --set-rpath` with
the interpreter's directory, the `lib` dirs of the dependency closure (§ `exports`) and the
file's own `DT_RUNPATH` (or `DT_RPATH`) entries, in that order, each once, keeping only entries
that start with `/` or `$` and whose second character is not `_` so an earlier pass's padding
drops out. The path ends in `:/` and `entries × 48 - 1` underscores, the same slack the driver's
links get (§ 8), so `reloc-fixup` can rewrite it in place. Files with a `PT_INTERP` also get
`--set-interpreter <dir>/./×12/<ld.so>` and, when `--stub` is given, `--set-entry-stub`. The
stub only works after `reloc-fixup`, so callers pass it for `$out` and not for an unpacked
tree whose binaries run during the install. Each `formatelf` line is echoed with a `+` prefix,
and one `implant:` note names each rewritten file, sorted.

`launchers` walks `bin/` of the output in name order, skipping dotfiles, dangling links, entries
that already have a `.<name>.launch` record and symlinks whose target sits in `bin/` (aliases
like `python3 -> python3.14`). Records for scripts carry PATH prepended with each direct
dependency's `bin/` plus every dependency's `exports.env` as `default`s, records for binaries
only the `exports.env` part, since a binary never needs a library's tools on PATH and the
launcher costs an execve. Values have the output replaced by `{root}` and the store by
`{store}`. A file starting with `#!` is a script, its first line split on whitespace with a
leading `env` dropped. `/bin/sh` needs no launcher unless there is script env to inject. An
interpreter under the store that neither the output nor a direct dependency owns is a leaked
build tool, the line is rewritten to
`#!/usr/bin/env <name>` with a `script:` note and the file left alone. A bare name is looked up
as `bin/<name>` in the output and its direct dependencies, never on the build PATH, and a line
into the output itself is rewritten to the `env` form because it would dangle. A 64-bit ELF with
a `PT_INTERP` under `--ldso` runs as `<ld.so> --argv0 {self} --library-path <interp dir and
closure lib dirs> {root}/bin/.<name>`, any other ELF gets a record only when a dependency
exports `env`, with `argv0` set to `{argv0}`. The file moves to `bin/.<name>`, the record
`{ env, program, args | argv0 }` is written to `bin/.<name>.launch`, `bin/<name>` becomes a
symlink to the launch binary and a `launcher:` note says what it runs. A symlinked script is
run by its resolved path so its `$0` logic holds.

## 12. nix-store mode

`mummer nix-store` speaks the Nix daemon's builder RPC over `NIX_REMOTE=unix://<socket>` with
protocol 1.38 and the features `realisation-with-path-not-hash`, `disable-set-options`,
`add-to-store-scanning` and `submit-output`. `add-text <name> [references…]` adds stdin as a
text path and prints it. `add-drv <name>` reads a derivation as JSON, serialises it as ATerm
and adds it. `fetchurls` reads a JSON array of `{ name, url, algo, hex, sri }`, adds one
`builtin:fetchurl` derivation each and prints `[ { drv, out } ]`. `submit <drv> <output>`
registers an output. With `MUMMER_NIX_STORE_OFFLINE` set, paths are computed locally and nothing
is submitted.

## 13. bootstrap mode

`mummer bootstrap <recipe>` is the builder of every stage derivation in bootstrap/default.nix. It
reads the structured attrs, exports each one except `outputs`, `args` and `builder` as an
environment variable for the recipe's children (lists space-joined, booleans and numbers as
text), sets `out`, `MUMMER_LOG` to `<build top>/mummer.log` and `MUMMER_STORE_ROOTS` to every attr that
names a store path plus the sysroot's `roots` file, runs the recipe, prints `== cache:
<kind>=<count>…` from the log, and clears group and other write bits on `$out`. The recipes are
`mummer`, `launch`, `dlaudit`, `linux-headers`, `sysroot`, `apple-sdk`, `musl`, `glibc`,
`mingw-w64`, `vcruntime`, `compiler-rt`, `runtimes`, `cc`, `cmake` and `llvm`, each a `Recipe`
in `mummer/src/bootstrap/`. A libc recipe
writes `etc/cc/include-dirs`, and an SDK adds `etc/cc/flags` and `etc/cc/cxxflags` with the
words `SYSROOT` and `LLD` standing for the final paths, which `compiler-rt` and `cc` read.

## 14. fetch modes

`mummer fetch <kind>` is the producer of every lock-file fetcher in nix/fetch.nix, run under
`builder-rpc-v0` with its attrs as environment. A producer reads the lock out of `source`
(`root` below it), builds one `builtin:fetchurl` row per artefact from the hash the lock
carries, adds them through the daemon, and submits a collecting derivation as its own output.
The kinds are `cargo`, `npm`, `pnpm`, `yarn`, `bun`, `deno` (then `deno-files`, a second
stage over the fetched jsr manifests), `pypi` (uv.lock, followed by `pypi-vendor` for Rust
dependencies in sdists and source trees), `gems`, `hex`, `go`, `hackage`, `luarocks`
and `winsdk`, each a `Producer` in `mummer/src/fetch/`. The collecting derivation is
content-addressed and runs `mummer assemble`, which reads its `layout` from the structured attrs,
each entry `{ to, link | unpack | write | copy+append }`, with bsdtar and chmod from `peanut`.
`winsdk` submits `mummer winsdk-assemble` instead, which unpacks the .vsix and .msi payloads with
7zz and writes the sysroot's `etc/cc` facts. `mummer unpack` is nix/sources.nix's fixed-output
builder, it downloads the first of `urls` that answers (rustls, built-in roots, proxies from the
environment) and unpacks it into `out` the way spieler's prefetch does through the same mummer unpack, so their hashes agree.

Python fetching accepts dependency groups, extras, marker overrides, sdist preferences and
source trees. The vendor stage carries explicit references to those trees and places each
package's Cargo dependencies under `vendor/<name>/`.
