use core.nu [ctx]

# configure runs the bindist's own ghc-toolchain-bin and `make install` runs ghc-pkg, so it has
# to run here before finish implants $out. Debian's .deb is installed as it comes
export def install []: nothing -> nothing {
   let c = (ctx)
   if ($"($c.src)/usr/lib/ghc" | path exists) {
      ^bigtop system ghc-install --deb
   } else {
      implant $c $c.src
      ^bigtop system ghc-install
   }
}
