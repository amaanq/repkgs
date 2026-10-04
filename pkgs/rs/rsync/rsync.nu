use core.nu *

# fix-shebangs skips the non-executable test scripts
export def test-scripts []: nothing -> nothing {
   let c      = (ctx)
   let python = (tool python3)
   let bash   = (tool bash)

   fix-shebangs $c.src
   for file in (files $"($c.src)/testsuite/*.py") {
      edit $file { str replace --all '#!/usr/bin/env python3' $"#!($python)" | str replace --all '#!/usr/bin/env bash' $"#!($bash)" }
   }
}
