use core.nu *

# the cross builds of clang's dependents run the build machine's tblgen from here
export def keep-tblgen []: nothing -> nothing {
  let c = (ctx)
  cp $"bin/clang-tblgen($c.platform.ext.exe)" $"($c.out)/bin/"
}
