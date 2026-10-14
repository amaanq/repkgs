use core.nu [ctx cue]

# the test programs and module playground are build_by_default: false, so plain ninja skips them
export def test-prereq []: nothing -> nothing {
   let c = (ctx)
   cue ninja -C $c.build $"-j($c.njobs)" meson-test-prereq
}
