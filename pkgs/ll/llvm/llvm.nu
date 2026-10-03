use core.nu *

# a cross build's llvm-config cannot run here, the native one beside it can answer for it
export def host-config []: nothing -> nothing {
   let host = $"((ctx).out)/host"
   x cmake --build NATIVE --target llvm-config
   mkdir $host
   cp NATIVE/bin/llvm-config $"($host)/llvm-config"
}
