use core.nu [act]

# --cc names the compiler curl was built with, by its store path
export def plain-cc []: nothing -> nothing {
   let script = $"((act).out)/bin/curl-config"
   let text   = (open --raw $script | str replace -r "echo '[^']*/bin/cc'" "echo 'cc'")
   $text | save -f $script
}
