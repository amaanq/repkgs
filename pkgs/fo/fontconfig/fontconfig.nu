use core.nu [act shebangs]

# the meson tests run scripts through /bin/bash, absent in the sandbox
export def test-shebangs []: nothing -> nothing {
   shebangs (act).src
}
