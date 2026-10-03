use core.nu *

export def relocate-compile-et []: nothing -> nothing {
   let c = (ctx)
   edit $"($c.out)/bin/compile_et" { str replace $"DIR=($c.out)" 'DIR=$(cd "$(dirname "$0")/.." && pwd -P)' }
}
