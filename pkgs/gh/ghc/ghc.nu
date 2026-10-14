use core.nu [act drv knobs dep bin cue glob-files]

def --wrapped hadrian [...targets: string]: nothing -> nothing {
   let c = (act)
   cd $c.src
   let flavour = $"release+no_profiled_libs+split_sections(if $c.spec.debug { '+debug_info' })"
   let program = (^cabal list-bin $"--project-dir=($c.src)/hadrian" ...(knobs cabal).flags exe:hadrian | str trim)
   cue $program --directory $c.src $"-j($c.njobs)" --docs=none $"--flavour=($flavour)" ...$targets
}

def linked [flag: string, dependency: string, headers: string]: nothing -> list<string> {
   let root = (dep $dependency "ghc links it")
   [$"--with-($flag)-includes=($root)/($headers)" $"--with-($flag)-libraries=($root)/lib"]
}

def configures-key []: nothing -> string {
   let given = (drv)
   ^bigtop probe key $"configures/($given.src | path basename)" ...$given.patches
}

export def --env configure []: nothing -> nothing {
   let c = (act)
   $env.CONFIG_SITE = (^bigtop probe configures-restore (configures-key) | str trim)
   cd $c.src
   let libraries = [(linked ffi libffi include) (linked gmp gmp include) (linked curses ncurses include/ncursesw)] | flatten
   # autoreconf would run aclocal, which needs automake, and AC_PATH_PROG keeps only absolute presets
   cue sh ./configure $"--prefix=($c.out)" --with-system-libffi ...$libraries GHC=ghc $"fp_cv_matching_ghc_pkg=($env.CABAL_GHC_PKG)" $"AutoreconfCmd=(bin autoconf)" ...(^bigtop system ghc-toolchain | lines)
}

export def build []: nothing -> nothing {
   hadrian binary-dist-dir
   ^bigtop probe configures-store (configures-key)
}

export def install []: nothing -> nothing {
   cd (glob-files --dirs $"((act).src)/_build/bindist/ghc-*" | first)
   ^bigtop system ghc-install
}
