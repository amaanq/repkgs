use core.nu *

# the PEP 517 backend is the python package in the source tree, installed beside the binary
export def python-backend []: nothing -> nothing {
  let c = (ctx)
  let site = $"($c.out)/lib/python3/site-packages"
  mkdir $site
  cp -r $"($c.src)/maturin" $site
}
