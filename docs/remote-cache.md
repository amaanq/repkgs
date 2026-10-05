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

The remote speaks Bazel's HTTP cache protocol (`GET`/`PUT` on `/ac/<sha256>` and `/cas/<sha256>`).
A key is an action cache entry whose digest is the sha256 of the key text. The entry is a real REAPI
`ActionResult`, so a server that validates entries, as bazel-remote does by default, takes it. A
value up to 1 MiB rides in the entry's `stdout_raw`, one round trip. A larger one is a CAS blob
named by its own sha256 that the entry's one output file points at, written before the entry so a
reader never sees an entry whose blob is missing, and dropped as a miss when its bytes do not hash
to its name.

The GETs a client pipelines in one `get_many` reach the remote together, up to 16 at a time, so a
cache hit costs two round trips (the manifest, then the result) however far the server is. Keys
under `tightrope::LOCAL_PREFIX` (`i/`, the stat-stamped file identities) name this machine's
files and never go to the remote.

## Failure

The remote never fails a build. An unreachable remote, a timeout or a refused write turns into a
local miss and one warning line in mummerd's log, and mummerd stops asking for a while before
trying again. A blob whose bytes do not hash to its name is dropped and counted as a miss.

## Configuration

`--remote <url>` (`MUMMERD_REMOTE`) and `--remote-token <file>` (`MUMMERD_REMOTE_TOKEN`), the file
holding `user:password` for HTTP basic auth. With a token mummerd writes through from a background
queue that never holds up a build, without one it only reads. The NixOS module takes
`services.mummerd.remote` and `services.mummerd.remoteTokenFile`, the latter a string handed over
with `LoadCredential` so the secret stays out of the store.

Serving it is bazel-remote, for example `bazel-remote --dir /var/cache/bazel-remote --max_size 500
--http_address 127.0.0.1:8080 --htpasswd_file htpasswd`, which only the htpasswd users can use.
Never expose that port directly. Basic auth over plain HTTP hands the write password, and with it
the power to plant object files in everyone's builds, to anyone on the path. Reach it over a
private network such as Tailscale, or through an HTTPS reverse proxy, and point `--remote` at the
`https://` address. bazel-remote has no read-only users, every htpasswd user can write, so on a
private network add `--allow_unauthenticated_reads` and give the password only to writers.
