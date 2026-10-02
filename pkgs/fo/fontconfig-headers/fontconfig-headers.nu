use core.nu *

export def header []: nothing -> nothing {
  let version = (open meson.build | parse -r "cacheversion = '(?<number>[0-9]+)'" | first | get number)
  open fontconfig/fontconfig.h.in | str replace "@CACHE_VERSION@" $version | save fontconfig/fontconfig.h
}
