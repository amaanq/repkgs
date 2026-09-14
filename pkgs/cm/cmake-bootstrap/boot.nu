use core.nu *
use probe-cache.nu

export def configure []: nothing -> nothing {
  let c = (ctx)
  let key = (probe-cache key cmake-bootstrap (files $"($c.src)/**/{CMakeLists.txt,*.cmake}"))
  let init = $"($c.build)/probe-init.cmake"
  let had = (probe-cache restore $key $init)
  note cmake-probes (if $had { "restored" } else { "cold" })
  (x $env.CONFIG_SHELL ./bootstrap $"--prefix=($c.out)" $"--parallel=($c.njobs)" --no-system-libs
    --no-qt-gui --docdir=share/doc/cmake --mandir=share/man ...(if $had { [$"--init=($init)"] } else { [] })
    -- -DCMAKE_USE_OPENSSL=OFF -DBUILD_TESTING=OFF -DCMake_BUILD_LTO=OFF $"-DCMAKE_SYSTEM_PREFIX_PATH=($c.platform.sysroot)")
  if not $had {
    probe-cache cmake-init CMakeCache.txt | save -f $init
    probe-cache store $key $init
  }
}
