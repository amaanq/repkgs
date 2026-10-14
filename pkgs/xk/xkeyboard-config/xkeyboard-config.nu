use core.nu [ctx fix-shebangs]

# meson runs the rules generators and test scripts by their /usr/bin/env shebangs, absent in the sandbox
export def shebangs []: nothing -> nothing {
   fix-shebangs (ctx).src
}
