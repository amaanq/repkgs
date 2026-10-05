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

The remote speaks REAPI over gRPC, as NativeLink serves it. A key is an action cache entry whose
digest is the sha256 of the key text, under the instance named by the URL's path. A value up to
1 MiB rides in the entry's `stdout_raw`, one round trip. A larger one is a CAS blob written through
ByteStream before the entry naming it, so a reader never sees an entry whose blob is missing, and
dropped as a miss when its bytes do not hash to its digest. NativeLink's own limit of 4 MiB per
gRPC message is why the cut sits there.

The GETs a client pipelines in one `get_many` reach the remote together, up to 16 at a time on one
HTTP/2 connection, so a cache hit costs two round trips (the manifest, then the result) however far the server is. Keys
under `tightrope::LOCAL_PREFIX` (`i/`, the stat-stamped file identities) name this machine's
files and never go to the remote.

## Failure

The remote never fails a build. An unreachable remote, a timeout or a refused call turns into a
local miss and one warning line in mummerd's log, and mummerd stops asking for a while before
trying again. Reads and writes rest apart, so a machine whose uploads are refused keeps its hits.

## Configuration

`--remote grpcs://host:port/instance` (`MUMMERD_REMOTE`), `grpc://` for plain HTTP/2 on a private
network. `--remote-ca` (`MUMMERD_REMOTE_CA`) is the CA the server's certificate chains to, else
the web's roots, and `--remote-identity` (`MUMMERD_REMOTE_IDENTITY`) a PEM file with this
machine's client certificate and key. Only `--remote-writes` uploads, from a background queue
that never holds up a build. The NixOS module takes `services.mummerd.remote`, `remoteCa`,
`remoteIdentityFile`, the last a string handed over with `LoadCredential` so the key stays out of
the store, and `remoteWrites`.

## Serving

NativeLink on scarp serves a `motley` instance on two listeners. The tailnet one takes writes from
your builders. The other has TLS with `client_ca_file`, so only holders of a client certificate
signed by that CA get in, and its action cache is `read_only`. NativeLink has no read-only CAS, but
CAS writes are hash-checked and nothing reaches the action cache from there, so a certificate holder
can at worst fill disk. nunatak forwards a public port to that listener as plain TCP, holding no
keys. Revoking a teammate means listing their certificate in `client_crl_file`.
