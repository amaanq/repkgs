use core.nu [act shebangs]

# make check execs its scripts through /usr/bin/env, absent in the sandbox
export def test-shebangs []: nothing -> nothing {
   shebangs (act).src
}
