use core.nu *

export def drop-rehash []: nothing -> nothing {
  rm $"((ctx).out)/bin/c_rehash"
}
