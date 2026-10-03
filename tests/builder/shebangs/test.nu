use checks.nu [expect pass]

let top  = (mktemp -d)
let tree = $"($top)/tree"
mkdir $tree
$env.NIX_BUILD_TOP = $top

let sh      = (which sh | get path.0)
let nushell = (which nu | get path.0)

# name, executable, contents, and what bigtop should leave behind
let cases = [
  [name           exec  before                                                  after];
  [env            true  "#!/usr/bin/env sh\ntrue\n"                             $"#!($sh)\ntrue\n"]
  [env-spaced     true  "#! /usr/bin/env sh -eu\ntrue\n"                        $"#!($sh) -eu\ntrue\n"]
  [absolute       true  "#!/usr/bin/nu --no-config-file\n1\n"                   $"#!($nushell) --no-config-file\n1\n"]
  [local          true  "#!/usr/local/bin/nu\n1\n"                              $"#!($nushell)\n1\n"]
  [bin-sh         true  "#!/bin/sh\ntrue\n"                                     "#!/bin/sh\ntrue\n"]
  [bin-env        true  "#!/usr/bin/env\n"                                      "#!/usr/bin/env\n"]
  [unknown        true  "#!/usr/bin/env no-such-interpreter\n"                  "#!/usr/bin/env no-such-interpreter\n"]
  [later-env      true  "#!/bin/sh\nexec nu -x \"$0\"\n#!/usr/bin/env nu\n1\n"  $"#!/bin/sh\nexec nu -x \"$0\"\n#!($nushell)\n1\n"]
  [later-absolute true  "#!/bin/sh\n#!/usr/bin/nu\n"                            "#!/bin/sh\n#!/usr/bin/nu\n"]
  [not-executable false "#!/usr/bin/env sh\ntrue\n"                             "#!/usr/bin/env sh\ntrue\n"]
]

for case in $cases {
  let file = $"($tree)/($case.name)"
  $case.before | save -f $file
  if $case.exec { ^chmod +x $file }
}
let stamp = (ls $"($tree)/env" | get modified.0)

^bigtop shebangs $tree
for case in $cases {
  let found = (open --raw $"($tree)/($case.name)")
  expect $"($case.name): ($found | lines | first) wanted ($case.after | lines | first)" ($found == $case.after)
}
expect "a rewrite keeps the file's mtime" ((ls $"($tree)/env" | get modified.0) == $stamp)

^bigtop shebangs --undo $tree
for case in $cases {
  expect $"($case.name) is restored by --undo" ((open --raw $"($tree)/($case.name)") == $case.before)
}

pass
