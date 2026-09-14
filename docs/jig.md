# jig and jigd, the contract

What the compiler wrapper and its daemon promise to the recipes, the builder and each other.
`design.md` says why they exist. This file says exactly what they do, so either side can be
replaced against it. Byte layouts and key shapes here are normative. Anything not written
here is an implementation choice.

## 1. Binaries and modes

**jig** is one static executable copied into every toolchain. It picks its mode from `argv[0]`,
or from the first argument when `argv[0]` is `jig`. `jig --help` and `jig <mode> --help` describe
the modes and their flags. The compiler, `rustcwrap`, `slot` and `header-key` modes hand their
arguments through untouched, every other mode parses them and fails on an unknown one.

| invoked as                                                                 | mode                                           |
| -------------------------------------------------------------------------- | ---------------------------------------------- |
| `cc`, `gcc`, `clang`, `cc-build`, `gcc-build`, any other name              | C driver, `cc`                                 |
| `c++`, `g++`, `clang++`, any name containing `++`                          | C++ driver, same code with C++ defaults        |
| `jig rustcwrap <rustc> <args…>`                                            | rustc wrapper, set as cargo's `RUSTC`          |
| `jig cache get\|put <key> <file>`                                          | one object, exit 1 on a miss, 2 with no daemon |
| `jig cache get-dir\|put-dir <key> <directory>`                             | a directory tree, same exits                   |
| `jig slot <program> [args…]`                                               | run under a build slot                         |
| `jig gocacheprog`                                                          | Go's `GOCACHEPROG` protocol on stdin/stdout    |
| `jig header-key <context> <warning-policy> <compiler> <args…>`             | autoconf header probe key                      |
| `jig reloc-fixup <directory> [--dest <store path>] [--deny <store path>]…` | post-link relocation                           |
| `jig nix-store add-text\|add-drv\|fetchurls\|submit …`                     | builder RPC to the Nix daemon                  |
| `jig shebangs <directory> [--undo]`                                        | point `#!/usr/bin/env` scripts at the build's env, or back |
| `jig exports <store path…>`, `jig deps <store path…>`                       | a package's exports with defaults, or the propagated closure, as JSON |
| `jig write-exports <output>`                                               | write `exports.json` from the spec on stdin, propagating .pc and cmake requirements |
| `jig prune <output>`                                                       | drop docs and junk, unpack man pages, print the inventory as JSON |
| `jig relativize <prefix> <store path>`                                     | make .pc files, foo-config scripts and links relative, fail on a leaked prefix |

**jigd** runs on the host, outside every sandbox, as `jigd <socket>`. It owns the compile
cache, the file identity cache and the build slots. Nothing in a derivation names it, so
outputs are identical with or without it.

The socket is `JIG_SOCK` when set, otherwise `<store parent>/var/nix/jigd/socket` where the
store parent is `/nix` for `/nix/store`. `tools/repkgs/repkgs cache start` maps it into sandboxes with
`extra-sandbox-paths`.

## 2. Configuration

### 2.1 `etc/jig.conf`

jig resolves `/proc/self/exe`, takes its grandparent as the installation root and reads
`<root>/etc/jig.conf`. Lines are `key = value`, blank lines and `#` comments allowed, unknown
keys are an error. Without the file jig is unconfigured and passes arguments through
unchanged, and `JIG_CC` must name the compiler.

| key          | meaning                                                                            |
| ------------ | ---------------------------------------------------------------------------------- |
| `cc`         | the real compiler, always clang                                                    |
| `binfmt`     | `elf`, `macho` or `coff`, default `elf`                                            |
| `libc`       | libc store path, `<libc>/lib` closes every RUNPATH and holds the interpreter       |
| `interp`     | interpreter file name under `<libc>/lib`, default `ld-linux-x86-64.so.2`           |
| `crt`        | `crt_interp` object, present when executables get the relocatable interpreter stub |
| `runtimes`   | directory added to RUNPATH when the link pulls a C++ or unwind runtime             |
| `flags`      | space separated, first on every command line                                       |
| `cxxflags`   | space separated, after `flags` for C++                                             |
| `prefix-map` | colon separated `from=to` pairs, each becomes `-ffile-prefix-map=`                 |

### 2.2 Environment read by jig

| variable                | meaning                                                                             |
| ----------------------- | ----------------------------------------------------------------------------------- |
| `JIG_CC`                | compiler when unconfigured                                                          |
| `JIG_SOCK`              | daemon socket path                                                                  |
| `JIG_STORE_IDENTITY`    | `content` enables content identity (§3.2), anything else is path identity           |
| `JIG_STORE_ROOTS`       | space separated store paths whose masked form may be restored on replay             |
| `JIG_STORE_DIR`         | compile-time only, the store prefix, default `/nix/store`                           |
| `out`                   | the derivation's own output path, its hash is masked in keys and objects            |
| `PKGS_CC`               | JSON `{ "<toolchain root>": { cflags, cxxflags, ldflags } }`, the package's flags   |
| `PKGS_PREFIX_MAP`       | colon separated `from=to`, appended to `prefix-map`                                 |
| `JIG_SLOT`              | set to a nonempty value by jig on the compiler it runs, so nested jigs take no slot |
| `JIG_LOG`               | append one TSV line per invocation (§10)                                            |
| `JIG_LOG_ARGS`          | append the argument line of each uncached invocation                                |
| `JIG_NIX_STORE_OFFLINE` | nonempty makes `nix-store` mode compute paths without a daemon                      |
| `NIX_BUILD_TOP`         | the slot owner, `-` when unset                                                      |
| `TMPDIR`                | link dependencies under it are not inputs                                           |
| `GOCACHEPROG_DIR`       | where `gocacheprog` materialises outputs, default `$TMPDIR/gocacheprog`             |

### 2.3 jigd options

`jigd <socket>` takes its options as flags, each with an environment variable that supplies
the value when the flag is absent. `jigd --help` lists them.

| flag          | variable         | meaning                                                   |
| ------------- | ---------------- | --------------------------------------------------------- |
|               | `XDG_CACHE_HOME` | packs live in `<it>/jigd/packs`, default `$HOME/.cache`   |
| `--size`      | `JIGD_SIZE`      | tier 0 budget in GiB, default 50, 0 means unlimited       |
| `--cold`      | `JIGD_COLD`      | a second tier directory that takes what tier 0 pushes out |
| `--cold-size` | `JIGD_COLD_SIZE` | its budget in GiB, default unlimited                      |
| `--slots`     | `JIGD_SLOTS`     | concurrent compiler slots, default the CPU count          |

## 3. Store paths, masking and identity

`STORE_DIR` is `/nix/store` unless built with `JIG_STORE_DIR`. A **store path** is any string
starting with `STORE_DIR/`.

### 3.1 Hash masking

`mask(text)` replaces every `STORE_DIR/<32 alphanumerics>-` with `STORE_DIR/*-`. Only the 32
characters directly after `STORE_DIR/` are considered, and only when a `-` follows them.
Masking is applied to keys, to stored text objects and to file listings so that a rebuilt
dependency with new store hashes and identical bytes still hits.

### 3.2 Identity modes

Path identity, the default, treats a store path as immutable and identifies it by name. The
input identity of a store file is `S:<path>`, and nothing is masked.

Content identity, `JIG_STORE_IDENTITY=content`, is what the builder sets. Store hashes are masked
everywhere, every input is identified by content, and `JIG_STORE_ROOTS` tells jig which real
paths a masked root may stand for.

### 3.3 Roots

A root is `STORE_DIR/<hash>-<name>` cut at the first `/` after the name. jig collects roots from
`JIG_STORE_ROOTS` and `$out`, keyed by their masked spelling. The first real path wins per
masked root. `resolve(masked)` maps `STORE_DIR/*-<name>/<rest>` back to the real root plus
`<rest>`, and fails when no root matches. `resolve_all(text)` masks the text and then replaces
each known masked root wherever it is followed by `/`, by one of `\t\n\:"';,)]}>`, or by the end
of the text.

### 3.4 Output placeholder

In content mode, when `$out` is a store path, its 32 character hash is replaced by 32 `e`
characters in keys, in cached objects and in cached diagnostics, and restored on replay. The
replacement is a byte substitution over the whole object, so it applies to binary objects as
well as text.

### 3.5 Input identity

`input_id(path)` is `S:<path>` for a store path in path mode. Otherwise it is
`C:<first 32 hex of blake3(bytes)>` where bytes are the file contents with the output placeholder
applied when the path is not a store path. jig asks the daemon for store file identities in
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
`-MP`, `-Wp,-MD,<x>`, `-Wp,-MMD,<x>` and the source file. Arguments jig injects from
`jig.conf` and `PKGS_CC` that the user did not pass are appended to `key_args`.

```
request = "cc/" + hash(
   field(tool_id(cc))
   field(key(cwd))
   field("link" | "link-one" | "compile")
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
`--print*`, `-dump*`), does not write `/dev/null`, `.pch` or `.gch`, and carries none of the
markers `@file`, `,@`, `-header`, `-Map`, `--print-map`, `--dependency-file`, `--out-implib`,
`--trace`, `--verbose`, `--print-gc-sections`, `--stats`, `-save-temps`, `-ftime-trace`,
`-fprofile`, `--coverage`, `-fmodules`, `-include-pch`, `-MJ`, `-serialize-diagnostics`,
`-gsplit-dwarf`, `,-M`, `,-t`. Links are cacheable for ELF only.

The manifest inputs are the depfile entries (jig adds `-MD -MF <tmp>` when the user asked for
none), lld's `--dependency-file` entries outside `TMPDIR` for links, the source and the object
inputs. The manifest environment is empty. A failing compile is stored only when it had inputs
and did not fail with `file not found`. A failing link is never stored.

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

`jig header-key` serves `builder/header-cache.nu`, which patches autoconf's C compile test to ask
jig before compiling. It only answers for `cc`, `gcc`, `cc-build` and `gcc-build` that resolve
to the running jig, a store path compiler, a last argument of `conftest.c`, arguments from a
fixed allow list, and none of `CCC_OVERRIDE_OPTIONS`, `CL`, `_CL_`, `GCC_EXEC_PREFIX`,
`COMPILER_PATH`, `SDKROOT` set.

First it looks the full compile up as a §5.2 request. A manifest hit whose result has status
`0` and empty diagnostics answers exit 0 without preprocessing. Otherwise it preprocesses with
`-E -MD -MT probe -Wunused-macros -fno-color-diagnostics -fno-caret-diagnostics` under
`LC_ALL=C`. `PACKAGE_*` and `VERSION` defines that clang reports unused are blanked in the source
text, any other diagnostic disqualifies the probe.

```
key = "probe/header/v1/" + full hex of hash(
   field(context) field(compiler) field(warning policy)
   for each expanded argument: field(arg)
   for each of CPATH C_INCLUDE_PATH CPLUS_INCLUDE_PATH OBJC_INCLUDE_PATH LIBRARY_PATH
               SOURCE_DATE_EPOCH LANG LC_ALL LC_CTYPE LC_MESSAGES: field(name) field(JSON of value)
   field(normalised source) field(preprocessed output)
   for each depfile entry except the source, canonical path: field(path) then field(bytes)
      unless the path is a store path)
```

Exit 0 when the daemon holds `yes\n` at that key, exit 3 with the key on stdout when it holds
nothing, exit 1 for every unsupported case. The autoconf side stores `yes\n` after a successful
compile. Only successful, warning free header checks are shared.

### 5.5 Go

`jig gocacheprog` speaks Go's cache program protocol. Action and output ids are 32 bytes, base64
on the wire, hex in keys. `go/a/<action hex>` holds the 32 output id bytes, `go/o/<output hex>`
holds the output. Bodies are materialised under `GOCACHEPROG_DIR`. Without a daemon every get
is a miss and every put is a local write.

### 5.6 Directories

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

One Unix socket connection per jig process, request lines terminated by `\n`, at most 65536
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

### 6.1 Frame

```
u32 native endian   uncompressed length
zstd                the object, level 1, single frame
```

Both the object and the frame are limited to 2 GiB. A reader rejects a frame shorter than 4
bytes, a decompressed size that differs from the header, and trailing bytes.

### 6.2 Identities

`IDS` is only asked for store paths that are not under `$out`. The daemon answers a path with the
`C:` identity of a regular file, cached by device, inode, size, mtime and ctime, and an empty line
for anything it cannot open. A client that gets an empty line hashes the file itself.

### 6.3 Slots

A slot is admission to run one compiler. jig takes one around every compiler it runs unless
`JIG_SLOT` is set, and sets `JIG_SLOT=1` on the child so nested wrappers do not. `jig slot`
does the same for arbitrary programs. `<build>` is `NIX_BUILD_TOP`, so the daemon can favour the
build holding the fewest slots when several are waiting. Ties go to the earliest request. A
client that disconnects while waiting is removed from the queue, and every slot a connection
holds is released when it closes.

## 7. jigd storage

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
records that are still current. Reads use `sendfile` from the pack.

### 7.2 Tiers and eviction

Tier 0 is `XDG_CACHE_HOME/jigd/packs` with the `JIGD_SIZE` budget, tier 1 is `JIGD_COLD` with
`JIGD_COLD_SIZE`. A tier over budget demotes packs, never the active one, choosing first any
pack with less than half its bytes live, then the least recently read, then the lowest id.
Demotion from the last tier deletes the pack. Otherwise the pack and hint are hard linked into
the next tier, or copied and renamed when linking fails, and removed from the old tier.

A pack's recency is its file mtime. A read touches the mtime at most once a minute, and a
demoted pack keeps its mtime, so recency survives restarts and moves. The demoter runs on its
own thread, woken by any put that takes tier 0 over budget, and is joined before the daemon
exits.

## 8. Driver argument policy

Given the user's arguments, jig builds the compiler command line in this order.

1. `--start-no-unused-arguments`
2. `flags` from `jig.conf`
3. `PKGS_CC.cflags` for this toolchain root, minus any `_FORTIFY_SOURCE` define when the user
   set one or compiles with `-O0`
4. for C++, `--driver-mode=g++`, `cxxflags`, `PKGS_CC.cxxflags`
5. `-ffile-prefix-map=<pair>` for each `prefix-map` and `PKGS_PREFIX_MAP` pair
6. for an ELF link with an input, `-Wl,--build-id=sha1` and `-Wl,--package-metadata={"type":"repkgs"}`
7. `--end-no-unused-arguments`
8. the user's arguments after the per format filter, with every rpath spelling removed for ELF
9. when linking with an input, `PKGS_CC.ldflags`
10. the link policy below, unless the user passed `-static`, `-static-pie`, `-r`, `-Wl,-r` or
    `-nostartfiles`

The per format filter drops `-Wl,--dynamic-linker*` and `-Wl,-dynamic-linker*` for ELF, drops
`-fPIC` `-fpic` `-fPIE` `-fpie` `-fno-PIC` `-fno-pic` `-pie` and raises `-std=c++98|03|0x|11`
and the `gnu++` spellings to 14 for COFF, and keeps everything for Mach-O.

**ELF link policy.** The RUNPATH is the user's rpath entries not under `/usr`, `/lib` or `/opt`,
verbatim, then one entry per store directory that satisfied a `-l` (a `-L` directory containing
`lib<x>.so` or `lib<x>.dylib`, or the directory of a store library named on the command line),
then `runtimes` when the link names `c++`, `stdc++`, `gcc_s`, `unwind` or `:libunwind.so` and
did not say `-nostdlib` or `-nostdlib++` (C++ links always), then `<libc>/lib`. Each store
entry is written as `<dir>/.:` and the path ends with `/` and `48 × max(entries, 1) + 80 ×
libraries − 1` underscores, which is the slack `reloc-fixup` rewrites into. The linker gets
`-Wl,--undefined-version`, `-Wl,-rpath,<that>` and `-Wl,--enable-new-dtags`. Executables
(not `-shared`) get `-Wl,--dynamic-linker=<libc>/lib/<interp>`, or with `crt` set, `-x none
<crt>`, the same interpreter spelled with twelve `./` segments so the stub can be written over
it, and `-Wl,--export-dynamic-symbol=__reloc_start`.

**Mach-O** links get `-Wl,-headerpad_max_install_names`. **COFF** links get nothing.

## 9. reloc-fixup

`jig reloc-fixup <prefix>` walks every regular file under the prefix, skipping `.debug` files.
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
`/` is searched in that order, failing the file when none has it. A library found in a directory
that does not also hold `libc.so.6` is rewritten to `$ORIGIN/<relative>/<name>` directly and its
directory leaves the RUNPATH. The RUNPATH string, then the direct NEEDED strings NUL separated,
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

Each jig invocation appends `tool\toutcome\tsubject\tmilliseconds` to `JIG_LOG`. `subject` is
the source, output or crate label with tabs and newlines replaced by spaces. After a cache
lookup that did not replay, the subject carries the reason after a space, so a miss can be
told apart from a first compile.

| reason | meaning |
|---|---|
| `new-key` | no manifest for the request |
| `inputs-changed:<path>` | that input's identity differs from the manifest, or it is gone |
| `env-changed:<name>` | that manifest variable differs |
| `object-gone` | the manifest matched but a result object was evicted |
| `replay-failed` | the daemon or the replay failed, see stderr |


| tool     | outcomes                                                                                                                             |
| -------- | ------------------------------------------------------------------------------------------------------------------------------------ |
| `cc`     | `cached`, `cached-error`, `compiled`, `compiled-error`, `compiled-unstored`, `failed`, `linked`, `uncacheable`, `no-daemon`, `query` |
| `rustc`  | `cached`, `compiled`, `compiled-unstored`, `failed`, `uncacheable`, `no-daemon`, `query`                                             |
| `header` | `compile-hit`, `hit`, `miss`, `unsupported-compiler`, `unsupported-options`, `preprocess-failed`, `diagnostics`                      |
| `go`     | `cached`, `compiled`, `compiled-unstored`                                                                                            |

`builder/finish.nu` tallies the file per package and treats `cached`, `cached-error`, `hit` and
`compile-hit` as hits. `JIG_LOG_ARGS` gets `outcome\t<arguments joined by spaces>` for every
non query invocation that ran the compiler.

## 11. Output tree modes

These serve `builder/prepare.nu` and `builder/finish.nu`, which used to walk the tree in nu.
Paths and errors follow the nu functions they replaced.

`shebangs` rewrites the first line of every executable regular file under 1 MiB that starts
with `#!/usr/bin/env` to `#!<env on PATH>`, writing in place and restoring the file's mtime.
`--undo` looks at every regular file, replaces that spelling on any line with `#!/usr/bin/env`,
and leaves mtimes alone.

`exports` prints one record per store path with the `exports.json` fields filled from the tree
when absent (§ `builder/exports.nu` documents the defaults), `env` values with `{root}` expanded,
and `root` added. `deps` prints the breadth first closure over `propagate`, each root once, in
discovery order. `write-exports` reads `{ name, exports, deps }`, where `exports` is the spec's
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

## 12. nix-store mode

`jig nix-store` speaks the Nix daemon's builder RPC over `NIX_REMOTE=unix://<socket>` with
protocol 1.38 and the features `realisation-with-path-not-hash`, `disable-set-options`,
`add-to-store-scanning` and `submit-output`. `add-text <name> [references…]` adds stdin as a
text path and prints it. `add-drv <name>` reads a derivation as JSON, serialises it as ATerm
and adds it. `fetchurls` reads a JSON array of `{ name, url, algo, hex, sri }`, adds one
`builtin:fetchurl` derivation each and prints `[ { drv, out } ]`. `submit <drv> <output>`
registers an output. With `JIG_NIX_STORE_OFFLINE` set, paths are computed locally and nothing
is submitted.
