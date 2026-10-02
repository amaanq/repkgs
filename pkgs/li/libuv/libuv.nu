use core.nu *

export def drop-static []: nothing -> nothing {
  let lib = $"((ctx).out)/lib"
  rm $"($lib)/libuv.lib" $"($lib)/pkgconfig/libuv-static.pc"
}
