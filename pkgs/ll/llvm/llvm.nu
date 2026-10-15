use core.nu [act cue]

# a cross build's llvm-config cannot run here, the native one beside it can answer for it
export def host-config []: nothing -> nothing {
   let host = $"((act).out)/host"
   cue cmake --build NATIVE --target llvm-config
   mkdir $host
   cp NATIVE/bin/llvm-config $"($host)/llvm-config"
}

export def check []: nothing -> nothing {
   let c = (act)
   with-env {LIT_OPTS: "--print-result-after=all"} {
      cue cmake --build $c.build --target check-llvm $"-j($c.njobs)"
   }
}
