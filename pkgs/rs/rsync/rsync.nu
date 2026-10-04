use core.nu *

# fix-shebangs skips the non-executable test scripts
export def test-scripts []: nothing -> nothing {
   let c      = (ctx)
   let python = (tool python3)

   fix-shebangs $c.src
   for file in (glob $"($c.src)/testsuite/*.py") {
      edit $file { str replace --regex '^#!/usr/bin/env python3' $"#!($python)" }
   }
}
