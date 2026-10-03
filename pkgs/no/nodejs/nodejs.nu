use core.nu *

const bundled = [zlib openssl zstd brotli libuv nghttp2 cares sqlite]

# --cross-compiling adds gyp's host toolset, whose ninja file defines js_protocol.stamp twice. With
# an emulator the code generators are built for the target and run under it instead
def cross-flags [platform: record]: nothing -> list<string> {
   if not $platform.cross { return [] }
   let generators = if ($platform.emulator | is-empty) { [--cross-compiling] } else { [--no-cross-compiling $"--emulator=($platform.emulator | str join ' ')"] }
   [$"--dest-cpu=($platform.names.gyp)" $"--dest-os=($platform.osNames.gyp)" ...$generators]
}

export def configure []: nothing -> nothing {
   let platform = (ctx).platform
   # node_mksnapshot faults under qemu-loongarch64 on pointers with bits 40 to 63 set
   let snapshot = if $platform.cpu == "loongarch64" { [--without-node-snapshot] } else { [] }
   let shared   = ($bundled | each {|library| $"--shared-($library)" })
   x python3 configure.py --prefix=/ --ninja ...$shared --with-intl=small-icu --without-corepack ...(cross-flags $platform) ...$snapshot
}

export def build []: nothing -> nothing { x ninja -C out/Release $"-j((ctx).njobs)" }

export def install []: nothing -> nothing {
   x python3 tools/install.py install --dest-dir "" --prefix (ctx).out --build-dir out/Release
}
