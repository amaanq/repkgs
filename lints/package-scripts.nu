#!/usr/bin/env nu
# Parses the build script a package's derivation runs (`pkg.script`) before any build does, so a
# module phase with the wrong signature or a misspelt command fails here. Each argument is a file
# in a package directory. Instantiating copies the builder and the modules a script uses into the store.
def main [...files: path] {
  let root = ($env.FILE_PWD | path dirname)
  let names = ($files | each { path dirname | path basename } | uniq)
  let quoted = ($names | each {|name| $'"($name)"' } | str join " ")
  let expr = $"let set = import ($root) { }; in map \(name: set.${name}.script or null\) [($quoted)]"
  let scripts = (^nix-instantiate --read-write-mode --eval --strict --json --option extra-experimental-features "ca-derivations dynamic-derivations" --expr $expr | from json)

  let failed = ($names | zip $scripts | where $it.1 != null | par-each {|pair|
    # the modules `use core.nu *` by bare name, and the derivation passes this include path
    let builder = ($pair.1 | parse -r '^use (\S+)/core\.nu' | get 0.capture0)
    # `zig stor` parses as an external, so each module's called phases are imported by name too
    let imports = ($pair.1 | parse -r '(?m)^use (?<path>\S+/(?<module>[^/\s]+)\.nu)$' | each {|used|
      let called = ($pair.1 | parse -r ('(?m)^\s*' + $used.module + ' (?<phase>\S+)') | get phase | uniq)
      if ($called | is-empty) { null } else { $"use ($used.path) [($called | str join ' ')]" }
    } | compact)
    let errors = ([$pair.1 ...$imports] | str join "\n"
      | ^$nu.current-exe --no-config-file $"--include-path=($builder)" "--experimental-options=[cell-path-types]" --ide-check 50 /dev/stdin
      | lines
      | each { from json }
      | where type == diagnostic and severity == Error
      | get message
      | uniq
      | each {|message| $"  ($message)" })
    if ($errors | is-empty) { null } else { $"($pair.0): generated build script\n($errors | str join "\n")" }
  } | compact)
  if ($failed | is-not-empty) {
    print -e ($failed | str join "\n")
    exit 1
  }
}
