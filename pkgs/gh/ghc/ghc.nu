use core.nu *

def --wrapped hadrian [...targets: string]: nothing -> nothing {
   let c = (ctx)
   cd $c.src
   let flavour = $"release+no_profiled_libs+split_sections(if $c.spec.debug { '+debug_info' })"
   let program = (^cabal list-bin $"--project-dir=($c.src)/hadrian" ...(options cabal).flags exe:hadrian | str trim)
   x $program --directory $c.src $"-j($c.njobs)" --docs=none $"--flavour=($flavour)" ...$targets
}

def linked [flag: string, dependency: string, headers: string]: nothing -> list<string> {
   let root = (dep-root $dependency "ghc links it")
   [$"--with-($flag)-includes=($root)/($headers)" $"--with-($flag)-libraries=($root)/lib"]
}

def configures-key []: nothing -> string {
   let given = (attrs)
   ^bigtop probe key $"configures/($given.src | path basename)" ...$given.patches
}

export def --env configure []: nothing -> nothing {
   let c = (ctx)
   cache-configures (configures-key)
   cd $c.src
   let libraries = [(linked ffi libffi include) (linked gmp gmp include) (linked curses ncurses include/ncursesw)] | flatten
   # autoreconf would run aclocal, which needs automake, and AC_PATH_PROG keeps only absolute presets
   x sh ./configure $"--prefix=($c.out)" --with-system-libffi ...$libraries GHC=ghc $"AutoreconfCmd=(tool autoconf)" ...(^bigtop system ghc-toolchain | lines)
}

export def build []: nothing -> nothing {
   hadrian binary-dist-dir
   store-configures
}

export def install []: nothing -> nothing {
   cd (files --dirs $"((ctx).src)/_build/bindist/ghc-*" | first)
   ^bigtop system ghc-install
}
