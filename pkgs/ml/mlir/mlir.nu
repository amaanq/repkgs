use core.nu *

export def keep-tblgen []: nothing -> nothing {
  cp bin/mlir-tblgen $"((ctx).out)/bin/"
}
