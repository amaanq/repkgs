use core.nu *

export def configure []: nothing -> nothing {
  let c = (ctx)
  # the venv's tooling group installs qemu's own python package with setuptools, wheel and
  # pip, and only the functional tests use it
  let wanted = (open --raw pythondeps.toml | lines | where {|line| $line !~ '^"(qemu|setuptools|wheel|pip)" =' })
  $wanted | str join "\n" | save -f pythondeps.toml
  # a stub for hosts whose GCC has no 16-byte cmpxchg, clang gets one from compiler-rt's
  # libcalls, so the generic CAS loop is what we want
  rm host/include/loongarch64/host/store-insert-al16.h.inc

  let targets = [aarch64 loongarch64 ppc64le riscv64 x86_64] | each {|cpu| $"($cpu)-linux-user" } | str join ","
  # --cross-prefix is what makes configure cross compile
  let cross = if $c.platform.cross {
    [$"--cross-prefix=($c.platform.gnuTriple)-" $"--host-cc=($env.CC_FOR_BUILD)"]
  } else { [] }
  cd $c.build
  # --prefix paths are templates that get_relocated_path() rebases on the binary
  with-env {PKG_CONFIG: "pkg-config"} {
    (x (tool sh) $"($c.src)/configure" --prefix=/usr --disable-download --without-default-features
      --enable-linux-user --disable-system --disable-tools --disable-docs --disable-werror
      $"--target-list=($targets)" $"--python=(which python3 | get 0.path)" ...$cross)
  }
}

export def build []: nothing -> nothing {
  let c = (ctx)
  x ninja -C $c.build $"-j($c.njobs)"
}

export def install []: nothing -> nothing {
  let c      = (ctx)
  let staged = $"($c.build)/dest"
  with-env {DESTDIR: $staged} { x ninja -C $c.build install }
  ^cp -r $"($staged)/usr/." $c.out
  # firmware and keymaps serve system emulation only
  rm -rf $"($c.out)/share/qemu"
}
