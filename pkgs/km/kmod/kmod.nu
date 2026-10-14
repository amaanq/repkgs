use core.nu [act cue]

# the test programs and module playground are build_by_default: false, so plain ninja skips them
export def test-prereq []: nothing -> nothing {
   let c = (act)
   cue ninja -C $c.build $"-j($c.njobs)" meson-test-prereq
}
