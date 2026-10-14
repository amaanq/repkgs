#!/usr/bin/env nu
# Parses each package's generated build script (`pkg.script`) without building it, so a phase with
# the wrong signature or a misspelt command fails here. Each argument is a file in a package
# directory. Instantiating copies the builder and the modules a script uses into the store.
def main [...files: path] {
   let root = ($env.FILE_PWD | path dirname)
   let names = ($files | each { path dirname | path basename } | uniq)
   let quoted = ($names | each {|name| $'"($name)"' } | str join " ")
   let expr = $"let set = import ($root) { }; in map \(name: set.${name}.script or null\) [($quoted)]"
   let flags = [--read-write-mode --eval --strict --json --option extra-experimental-features "ca-derivations dynamic-derivations"]
   let scripts = (^nix-instantiate ...$flags --expr $expr | from json)

   let failed = ($names | zip $scripts | where $it.1 != null | par-each {|pair|
      let name = $pair.0
      let script = $pair.1
      # the modules import core.nu by bare name, so it has to be on the include path
      let builder = ($script | parse -r '^use (?<dir>\S+)/core\.nu' | get 0.dir)
      # a misspelt `zig stor` parses as an external command, so each called phase is imported by name
      let phases = ($script | parse -r '(?m)^use (?<path>\S+/(?<module>[^/\s]+)\.nu)$' | each {|used|
         let called = ($script | parse -r ('(?m)^\s*' + $used.module + ' (?<phase>\S+)') | get phase | uniq)
         if ($called | is-not-empty) { $"use ($used.path) [($called | str join ' ')]" }
      } | compact)

      let errors = ([$script ...$phases] | str join "\n"
         | ^$nu.current-exe --no-config-file $"--include-path=($builder)" "--experimental-options=[cell-path-types]" --ide-check 50 /dev/stdin
         | lines
         | each { from json }
         | where type == diagnostic and severity == Error
         | get message
         | uniq)

      if ($errors | is-not-empty) {
         $"($name): generated build script\n($errors | each { $'  ($in)' } | str join "\n")"
      }
   } | compact)

   if ($failed | is-not-empty) {
      print -e ($failed | str join "\n")
      exit 1
   }
}
