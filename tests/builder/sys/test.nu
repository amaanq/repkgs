# bigtop syslibs over the lock files beside this script, which is what spieler records as act.lock's sys.
# The build-time half (check, env, go tags, gem flags) runs in every cargo, go, python and gems build
use checks.nu [expect pass]

$env.CARGO_HOME = (mktemp -d)
let wanted = (^bigtop syslibs wanted --tables $env.SYS_LIBS $env.fixtures | from json)
expect "wanted merges Cargo.lock without its optional and dev crates, go.sum and Gemfile.lock" ($wanted == [libyaml openssl sqlite])
let cargo = (^bigtop syslibs wanted-for --tables $env.SYS_LIBS cargo openssl-sys libsqlite3-sys serde | from json)
expect "wanted-for reads one ecosystem" ($cargo == [openssl sqlite])
pass
