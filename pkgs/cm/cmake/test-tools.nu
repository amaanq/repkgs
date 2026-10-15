use core.nu [act bin]

export def --env path []: nothing -> nothing {
   let sysroot = (act).platform.sysroot
   let bash    = (bin bash)

   $env.PKGS_SYSROOT = $sysroot

   mkdir sysroot-bin
   $"#!($bash)\nexec ($bash) ($sysroot)/bin/ldd \"$@\"\n" | save -f sysroot-bin/ldd
   chmod +x sysroot-bin/ldd
   $env.PATH = ($env.PATH | prepend [$"($env.PWD)/sysroot-bin" $"($sysroot)/sbin"])
}
