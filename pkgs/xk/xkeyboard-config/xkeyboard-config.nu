use core.nu [act shebangs]

# meson runs the rules generators and test scripts by their /usr/bin/env shebangs, absent in the sandbox
export def script-shebangs []: nothing -> nothing {
   shebangs (act).src
}
