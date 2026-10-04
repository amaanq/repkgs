# Remote compile cache

Out of the box, mummerd's cache is per machine. However, the remote tier behind mummerd shares it.

## Decided

- mummer, the keys and the derivations stay as they are. Only mummerd grows a remote tier.
- Lookup order is tier 0, the cold tier, then the remote. A remote hit is written into tier 0, so
  the next lookup stays local.
- Only machines holding write credentials store to the remote, which are yours and CI. Everyone
  else reads. Whoever writes supplies object files to every build that hits, so writing is a trust
  grant, not a convenience.
- The cache protocol only, no remote execution. Nix already spreads derivations over builders, and
  bigtop needs its local sandbox.

## How keys map

mummerd stores opaque values under string keys (`GET`, `HAS`, `PUT` in tightrope). A key is
already machine independent, store hashes are masked and `$out` is a placeholder, so the same
compile on two machines with the same toolchain asks for the same key.

On the remote a key is an action cache entry whose digest is the sha256 of the key text, and the
value is one blob in the content store, named by its own sha256. A `PUT` writes the blob first and
the action entry second, so a reader never sees an entry whose blob is missing. A `HAS` is one
action lookup. The batched `get_many` and `has_many` become batched remote calls, one round trip
per batch, since a round trip per compile would be slower than compiling small files.

## Failure

The remote never fails a build. An unreachable remote, a timeout or a refused write turns into a
local miss and one warning line in mummerd's log, and mummerd stops asking for a while before
trying again. A blob whose bytes do not hash to its name is dropped and counted as a miss.

## Configuration

`--remote <url>` (`MUMMERD_REMOTE`) and `--remote-token <file>` (`MUMMERD_REMOTE_TOKEN`). With a
token mummerd writes through, without one it only reads.

## Open, your call

- Protocol. Bazel's HTTP cache protocol (`GET`/`PUT` on `/ac/<sha256>` and `/cas/<sha256>`) needs
  only ureq, which mummer already links. The gRPC REAPI needs tonic, prost and h2, a much heavier
  dependency set for the same two operations. bazel-remote speaks both. Whether NativeLink serves
  the HTTP protocol has to be checked before choosing it as the server.
- Whether hits from the remote are written into the local cache of read-only machines (faster
  second build, more disk) or only served.
