use core.nu [ctx cue files]

export def install []: nothing -> nothing {
   let c = (ctx)
   cue sh install.sh $"--prefix=($c.out)" --disable-ldconfig
   let bookkeeping = [install.log uninstall.sh components rust-installer-version]
   rm -rf ...($bookkeeping | each {|file| $"($c.out)/lib/rustlib/($file)" }) ...(files --any $"($c.out)/lib/rustlib/manifest-*")
}
