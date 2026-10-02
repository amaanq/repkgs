use core.nu *

# --cc names the compiler curl was built with, by its store path
export def plain-cc []: nothing -> nothing {
  let script = $"((ctx).out)/bin/curl-config"
  let text = (open --raw $script | str replace -r "echo '[^']*/bin/cc'" "echo 'cc'")
  $text | save -f $script
}
