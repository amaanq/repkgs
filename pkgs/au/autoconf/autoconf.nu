use core.nu *

# the shipped man pages are older than the patched scripts, and help2man is not around to redo them
export def keep-man []: nothing -> nothing {
  for page in (files $"((ctx).src)/man/*.1") { ^touch $page }
}

# the installed scripts name m4, perl and bash by the build machine's paths
export def retarget-tools []: nothing -> nothing {
  let swaps = ([m4 perl bash] | each {|name|
    {built: (tool $name), runs: $"(dep-root $name 'the scripts run it')/bin/($name)"}
  })
  for script in (files --any $"((ctx).out)/bin/*") {
    edit $script {|| let text = $in; $swaps | reduce --fold $text {|swap, edited| $edited | str replace --all $swap.built $swap.runs } }
  }
}
