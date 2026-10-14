use core.nu [act cue]

# GitHub's tarball skips submodules, so we fill ext/ in ourselves
export def submodules []: nothing -> nothing {
   let ext = $"((act).src)/ext"

   cue cp -r $"($env.RAPIDCHECK_CATCH)/." $"($ext)/catch"
   cue cp -r $"($env.RAPIDCHECK_GOOGLETEST)/." $"($ext)/googletest"
   cue ln -s $env.RAPIDCHECK_BOOST $"($ext)/boost"
}
