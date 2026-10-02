use core.nu *

export def drop-preloader []: nothing -> nothing {
  rm $"((ctx).out)/lib/wine/x86_64-unix/wine-preloader"
}
