use core.nu [act cue]

# the release archive leaves the third_party/googletest submodule empty, and the tests build it from source
export def googletest []: nothing -> nothing {
   cue cp -r $"($env.BINARYEN_GTEST)/." $"((act).src)/third_party/googletest"
}
