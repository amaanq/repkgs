use core.nu [act cue]

export def host-config []: nothing -> nothing {
   let host = $"((act).out)/host"
   cue cmake --build NATIVE --target llvm-config
   mkdir $host
   cp NATIVE/bin/llvm-config $"($host)/llvm-config"
}

export def test []: nothing -> nothing {
   let c       = (act)
   let lit     = ($c.build | path join utils lit | path exists)
   let targets = [$"check-($c.spec.cmake.root)"] | append (if $lit { ["check-lit"] } else { [] })
   with-env {LIT_OPTS: "--print-result-after=all"} {
      cue cmake --build $c.build --target ...$targets $"-j($c.njobs)"
   }
}
