use core.nu *

export def build []: nothing -> nothing {
   let c = (ctx)
   do {
      cd $c.src
      (x python3 build/gen.py --no-last-commit-position --no-strip --no-static-libstdc++
         --allow-warnings $"--out-path=($c.build)")
   }
   # gen.py reads these from git history, which a tarball has none of
   let position = {number: 2572, commit: "3c5809c3"}
   [
      $"#define LAST_COMMIT_POSITION_NUM ($position.number)"
      $"#define LAST_COMMIT_POSITION \"($position.number) \(($position.commit)\)\""
      ""
   ] | str join "\n" | save $"($c.build)/last_commit_position.h"
   x ninja -C $c.build $"-j($c.njobs)" gn
}
