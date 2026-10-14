use core.nu [act dep bin rewrite glob-files]

# the shipped man pages are older than the patched scripts, and help2man is not around to redo them
export def keep-man []: nothing -> nothing {
   for page in (glob-files $"((act).src)/man/*.1") { ^touch $page }
}

# the installed scripts name m4, perl and bash by the build machine's paths
export def retarget-tools []: nothing -> nothing {
   let swaps = ([m4 perl bash] | each {|name|
      {built: (bin $name), runs: $"(dep $name 'the scripts run it')/bin/($name)"}
   })
   for script in (glob-files --any $"((act).out)/bin/*") {
      rewrite $script {|| let text = $in; $swaps | reduce --fold $text {|swap, edited| $edited | str replace --all $swap.built $swap.runs } }
   }
}
