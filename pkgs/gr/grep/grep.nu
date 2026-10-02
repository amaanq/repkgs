use core.nu *

export def drop-wrappers []: nothing -> nothing {
  let bin = $"((ctx).out)/bin"
  rm $"($bin)/egrep" $"($bin)/fgrep"
}
