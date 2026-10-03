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

export def configure []: nothing -> nothing {
   let c = (ctx)
   cd $c.src
   let libraries = [(linked ffi libffi include) (linked gmp gmp include) (linked curses ncurses include/ncursesw)] | flatten
   # autoreconf would run aclocal, which needs automake, and AC_PATH_PROG keeps only absolute presets
   x sh ./configure $"--prefix=($c.out)" --with-system-libffi ...$libraries GHC=ghc $"AutoreconfCmd=(tool autoconf)" ...(^bigtop system ghc-toolchain | lines)
}

# leaves $out out of the key since it moves with every recipe edit, and _build never embeds it
def cache-key []: nothing -> string {
   let given = (attrs)
   ^bigtop probe key $"hadrian/($given.src | path basename)" ...$given.patches --no-out
}

export def build []: nothing -> nothing {
   let tree     = $"((ctx).src)/_build"
   let key      = (cache-key)
   let restored = (^bigtop probe restore-dir $key $tree | complete).exit_code == 0
   note hadrian-cache (if $restored { "restored" } else { "cold" })
   hadrian binary-dist-dir
   ^bigtop probe store-dir $key $tree
}

export def install []: nothing -> nothing {
   cd (files --dirs $"((ctx).src)/_build/bindist/ghc-*" | first)
   ^bigtop system ghc-install
}
