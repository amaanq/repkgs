use core.nu *

export def --env configure []: nothing -> nothing {
  let c = (ctx)
  let suffix = if $c.platform.os == "macos" { "dylib" } else { "so" }
  [HAVE_SECCOMP_FILTER HAVE_SANDBOX_INIT HAVE_PLEDGE] | each {|feature| $"($feature)=0" } | append "" | str join "\n" | save configure.local
  # the script needs an echo that expands \n, which dash's does and bash's does not
  (x sh ./configure $"PREFIX=($c.out)" $"MANDIR=($c.out)/share/man" LINK_METHOD=shared
    $"LINKER_SOSUFFIX=($suffix)")
  # the regression tests run against the freshly built library
  $env.LD_LIBRARY_PATH = $env.PWD
}
