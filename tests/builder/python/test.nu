use checks.nu [expect pass split-debug]
use glob.nu [glob-files]

let extension = (glob-files $"($env.pkg)/lib/python3*/site-packages/tpy/_speed*.so" | first | path relative-to $env.pkg)
split-debug $extension
expect "the console script is a launcher symlink" (($"($env.pkg)/bin/tpy-hello" | path type) == symlink)
expect "no bytecode records a /build path"        ((^grep -rl /build $"($env.pkg)/lib" | complete).stdout == "")
pass
