use core.nu [act cue drv]

# its meson looks `nix` up on PATH, so the functional tests run after install
export def test []: nothing -> nothing {
   let c   = (act)
   let nix = (drv).package
   let dir = $"($c.build)/functional"

   $env.PATH            = ($env.PATH | prepend $"($nix)/bin")
   $env.PKG_CONFIG_PATH = $"($nix)/lib/pkgconfig:($env.PKG_CONFIG_PATH? | default '')"

   let machine = if ($"($c.build)/cross.ini" | path exists) {
      [--cross-file $"($c.build)/cross.ini" --native-file $"($c.build)/native.ini"]
   } else {
      []
   }

   cue meson setup   ...$machine $dir $"($c.src)/tests/functional"
   cue meson test -C $dir --print-errorlogs --num-processes $"($c.njobs)"
}
