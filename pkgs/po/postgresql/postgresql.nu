use core.nu [ctx edit tools-by-name]

# the compiled-in directories become /usr templates that the relocation code rebases
export def fixed-paths []: nothing -> nothing {
   edit $"((ctx).src)/src/include/meson.build" { str replace --all ", dir_prefix / dir_" ", '/usr' / dir_" }
}

export def relocate-pgxs []: nothing -> nothing {
   let c = (ctx)
   edit $"($c.out)/lib/postgresql/pgxs/src/Makefile.global" {
      str replace $"prefix := ($c.out)" 'prefix := $(abspath $(dir $(lastword $(MAKEFILE_LIST)))../../../..)'
      | tools-by-name
   }
}
