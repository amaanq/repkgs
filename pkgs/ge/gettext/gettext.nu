use core.nu [act knobs cue]

# the relocatable libtool installs only under a libdir ending in the configured one, so stage with
# DESTDIR and move the configured prefix into place
export def install []: nothing -> nothing {
   let c      = (act)
   let prefix = (knobs autotools | get flags | parse '--prefix={dir}' | get 0.dir)
   let stage  = $"($c.build)/stage"

   cue make -C $c.build install $"DESTDIR=($stage)"
   ^mv $"($stage)($prefix)" $c.out
}
