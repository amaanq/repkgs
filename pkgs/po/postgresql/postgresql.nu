use core.nu [act rewrite unstore]

# the compiled-in directories become /usr templates that the relocation code rebases
export def fixed-paths []: nothing -> nothing {
   rewrite $"((act).src)/src/include/meson.build" { str replace --all ", dir_prefix / dir_" ", '/usr' / dir_" }
}

export def relocate-pgxs []: nothing -> nothing {
   let c = (act)
   rewrite $"($c.out)/lib/postgresql/pgxs/src/Makefile.global" {
      str replace $"prefix := ($c.out)" 'prefix := $(abspath $(dir $(lastword $(MAKEFILE_LIST)))../../../..)'
      | unstore
   }
}
