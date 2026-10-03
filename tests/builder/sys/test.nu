# hoist syslibs over the lock files beside this script, which is what plumb records as [pin] sys.
# The build-time half (check, env, go tags, gem flags) runs in every cargo, go, python and gems build
use checks.nu *

let wanted = (^hoist syslibs wanted --tables $env.SYS_LIBS $env.fixtures | from json)
expect "wanted merges Cargo.lock, go.sum and Gemfile.lock" ($wanted == [libyaml openssl sqlite])
let cargo = (^hoist syslibs wanted-for --tables $env.SYS_LIBS cargo openssl-sys libsqlite3-sys serde | from json)
expect "wanted-for reads one ecosystem" ($cargo == [openssl sqlite])
pass
