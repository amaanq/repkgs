use checks.nu *

let python   = $"($env.pkg)/bin/python3"
let greeting = (with-env {REPKGS_PROJECT: $env.src} { ^$python -c "import tproj; tproj.main()" } | str trim)
expect "devEnv imports the checkout REPKGS_PROJECT names" ($greeting == "tproj-hello 1.0")
expect "devEnv cannot import the project without it"      ((do { ^$python -c "import tproj" } | complete).exit_code != 0)
pass
