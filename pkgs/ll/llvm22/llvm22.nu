use core.nu [ctx cue]

# a cross build's llvm-config cannot run here, the native one beside it can answer for it
export def host-config []: nothing -> nothing {
   let host = $"((ctx).out)/host"
   cue cmake --build NATIVE --target llvm-config
   mkdir $host
   cp NATIVE/bin/llvm-config $"($host)/llvm-config"
}
