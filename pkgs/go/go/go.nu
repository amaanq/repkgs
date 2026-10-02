use core.nu *

export def build []: nothing -> nothing {
  let c      = (ctx)
  let target = $"($c.platform.osNames.go)_($c.platform.names.go)"
  load-env {
    GOROOT_BOOTSTRAP: (which go | get 0.path | path dirname | path dirname)
    GOROOT_FINAL: $c.out
    GOCACHE: $"($c.build)/go-cache"
    GOFLAGS: "-trimpath"
    # jib's debug split keys on a GNU build-id
    GO_LDFLAGS: "-B gobuildid"
    GOOS: $c.platform.osNames.go
    GOARCH: $c.platform.names.go
    CC: (if $c.platform.cross { $env.CC_FOR_BUILD } else { "cc" })
  }
  if $c.platform.cross { $env.CC_FOR_TARGET = "cc" }
  if $c.cache { $env.GOCACHEPROG = (which gocacheprog | get 0.path) }
  do { cd src; x sh make.bash }

  # a cross make.bash leaves both toolchains, the target's under <os>_<arch>/
  rm -rf pkg/obj pkg/bootstrap
  if $c.platform.cross {
    rm -rf ...(ls bin pkg/tool | get name | where {|entry| ($entry | path basename) != $target })
    mv ...(files --any $"bin/($target)/*") bin/
    rm -rf $"bin/($target)"
  }
  mkdir $c.out
  for entry in [bin pkg src lib misc go.env VERSION] { ^cp -r $entry $c.out }
  for testdata in (files --any $"($c.out)/src/**/testdata") { rm -rf $testdata }
}
