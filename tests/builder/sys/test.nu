# jib syslibs over the lock files next to this script, what uptrack writes into [pin] sys.
# The build-time side (check, env, go tags, gem flags) is jib's syslibs too and runs in every
# cargo, go, python and gems package build
use checks.nu *

let dir = $env.fixtures
assert "wanted: union over Cargo.lock, go.sum, Gemfile.lock" ((^jib syslibs wanted --tables $env.SYS_LIBS $dir | from json) == [libyaml openssl sqlite])
assert "wanted-for: one ecosystem" ((^jib syslibs wanted-for --tables $env.SYS_LIBS cargo openssl-sys libsqlite3-sys serde | from json) == [openssl sqlite])
done
