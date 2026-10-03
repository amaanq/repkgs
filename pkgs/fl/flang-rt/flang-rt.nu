use core.nu *

# A gfortran beside the runtime: mummer itself, configured to drive flang with this runtime's paths.
# "@" is mummer's own prefix, so the configuration still holds once the output moves to the store
export def driver []: nothing -> nothing {
  let c      = (ctx)
  let cc     = $env.TOOLCHAIN
  let inside = {|pattern| files --dirs $"($c.out)/($pattern)" | first | str replace $c.out "@" }
  let flags  = [
    $"-B($cc)/bin" ...($c.spec.cmake.defs.CMAKE_Fortran_FLAGS | split row " ")
    $"-resource-dir=($c.platform.sysroot)/lib/clang" -rtlib=compiler-rt
    -fintrinsic-modules-path (do $inside "lib/clang/*/finclude/flang/*") $"-L(do $inside 'lib/clang/*/lib/*')"
  ]
  mkdir $"($c.out)/bin" $"($c.out)/etc"
  (open $"($cc)/etc/mummer.json" | reject flags cxxflags prefix-map
    | merge {fc: $c.spec.cmake.defs.CMAKE_Fortran_COMPILER, fflags: $flags}
    | save $"($c.out)/etc/mummer.json")
  cp $"($cc)/etc/roots" $"($c.out)/etc/"
  cp $"($cc)/bin/mummer" $"($c.out)/bin/"
  for name in [gfortran flang fortran] { ^ln -s mummer $"($c.out)/bin/($name)" }
}

export def hello []: nothing -> nothing {
  let c = (ctx)
  [ "program hello" " use iso_fortran_env" " print *, compiler_version()" "end program" "" ] | str join "\n" | save hello.f90
  x $"($c.out)/bin/gfortran" hello.f90 -o hello
  if not $c.platform.cross { x ./hello }
}
