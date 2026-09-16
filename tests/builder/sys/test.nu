# uptrack's sys-libs.nu over the lock files next to this script, what uptrack writes into [pin] sys.
# The build-time side (check, env, go tags, gem flags) is jib's syslibs and runs in every
# cargo, go, python and gems package build
use sys-libs.nu
use checks.nu *

let dir = $env.fixtures
assert "wanted: union over Cargo.lock, go.sum, Gemfile.lock" ((sys-libs wanted $dir) == [libyaml openssl sqlite])
assert "wanted-for: one ecosystem" ((sys-libs wanted-for cargo [openssl-sys libsqlite3-sys serde]) == [openssl sqlite])
done
