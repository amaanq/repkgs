use core.nu [act bin cue glob-files]

export def install []: nothing -> nothing {
   let c       = (act)
   let rustlib = $"($c.out)/lib/rustlib"
   for component in ([.] | append ($env.components | split row " ")) {
      cue sh $"($component)/install.sh" $"--prefix=($c.out)" --disable-ldconfig
   }
   # install.sh's own bookkeeping names the prefix
   rm -rf $"($rustlib)/($c.platform.rustTriple)/bin" $"($c.out)/etc" $"($rustlib)/install.log" ...(glob-files $"($rustlib)/manifest-*")
   # since 1.90 rustc links x86_64-linux-gnu through its self-contained gcc-ld/ld.lld, build
   # scripts too, so that has to be our lld
   let gcc_ld = $"($rustlib)/($c.platform.rustTriple)/bin/gcc-ld"
   mkdir $gcc_ld
   ^ln -s (bin ld | path expand) $"($gcc_ld)/ld.lld"
}
