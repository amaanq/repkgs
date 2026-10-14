use core.nu [act cue glob-files]

export def install []: nothing -> nothing {
   let c = (act)
   cue sh install.sh $"--prefix=($c.out)" --disable-ldconfig
   let bookkeeping = [install.log uninstall.sh components rust-installer-version]
   rm -rf ...($bookkeeping | each {|file| $"($c.out)/lib/rustlib/($file)" }) ...(glob-files --any $"($c.out)/lib/rustlib/manifest-*")
}
