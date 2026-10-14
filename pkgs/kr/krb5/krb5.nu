use core.nu [act rewrite]

export def relocate-compile-et []: nothing -> nothing {
   let c = (act)
   rewrite $"($c.out)/bin/compile_et" { str replace $"DIR=($c.out)" 'DIR=$(cd "$(dirname "$0")/.." && pwd -P)' }
}
