use core.nu [act cue glob-files emit]

export def configure []: nothing -> nothing {
   let c = (act)
   let key = (^bigtop probe key cmake-bootstrap ...(glob-files $"($c.src)/**/{CMakeLists.txt,*.cmake}"))
   let init = $"($c.build)/probe-init.cmake"
   let had = (try { ^bigtop probe restore $key $init; true } catch { false })
   emit cmake-probes (if $had { "restored" } else { "cold" })
   (cue $env.CONFIG_SHELL ./bootstrap $"--prefix=($c.out)" $"--parallel=($c.njobs)" --no-system-libs
      --no-qt-gui --docdir=share/doc/cmake --mandir=share/man ...(if $had { [$"--init=($init)"] } else { [] })
      -- -DCMAKE_USE_OPENSSL=OFF -DBUILD_TESTING=OFF -DCMake_BUILD_LTO=OFF $"-DCMAKE_SYSTEM_PREFIX_PATH=($c.platform.sysroot)")
   if not $had {
      ^bigtop probe cmake-init CMakeCache.txt | save -f $init
      ^bigtop probe store $key $init
   }
}
