use core.nu [ctx options x]

# the relocatable libtool installs only under a libdir ending in the configured one, so stage with
# DESTDIR and move the configured prefix into place
export def install []: nothing -> nothing {
   let c      = (ctx)
   let prefix = (options autotools | get flags | parse '--prefix={dir}' | get 0.dir)
   let stage  = $"($c.build)/stage"

   x make -C $c.build install $"DESTDIR=($stage)"
   ^mv $"($stage)($prefix)" $c.out
}
