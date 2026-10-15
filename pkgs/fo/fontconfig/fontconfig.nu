use core.nu [act shebangs]

# meson keeps the interpreter a script names when configure finds it, and
# /bin/bash is absent in the sandbox. The tarball ships the script unexecutable
export def test-shebangs []: nothing -> nothing {
   let test = (act).src | path join test
   chmod +x ($test | path join run-test.sh)
   shebangs $test
}
