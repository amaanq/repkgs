use core.nu [ctx fix-shebangs]

# make check execs its scripts through /usr/bin/env, absent in the sandbox
export def test-shebangs []: nothing -> nothing {
   fix-shebangs (ctx).src
}
