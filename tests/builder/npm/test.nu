use checks.nu *

expect "the bin entry is a launcher symlink"                   (($"($env.pkg)/bin/test-npm" | path type) == symlink)
expect "the installed script still starts with #!/usr/bin/env" ((open --raw $"($env.pkg)/lib/node_modules/test-npm/cli.js" | lines | first) == "#!/usr/bin/env node")
expect "the debug output is there but empty"                   ((ls $env.debug | length) == 0)
pass
