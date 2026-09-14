# PEP 517 wheel build and install, jig system phase python <verb>
use ../core.nu *

export def --env setup []: nothing -> nothing { load-env (^jig system setup python | from json) }
export def workdir []: nothing -> string { ^jig system workdir python | str trim }
export def build []: nothing -> nothing { ^jig system phase python build }
export def install []: nothing -> nothing { ^jig system phase python install }
export def test []: nothing -> nothing { ^jig system phase python test }

def site-packages [roots: list<string>]: nothing -> list<string> { $roots | each {|r| files --dirs $"($r)/lib/python3*/site-packages" } | flatten }

# those of the python build tools on PATH (PEP 517 backends and friends) and of what they
# propagate, pyapp.nu's PYTHONPATH
export def tool-site-packages []: nothing -> list<string> {
  site-packages (dep-closure ($env.PATH | each { path dirname } | where { (site-packages [$in]) | is-not-empty }) | get root)
}
