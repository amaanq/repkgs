# PEP 517 wheel build and install, jig system phase python <verb>
use ../core.nu *

export def --env setup []: nothing -> nothing { load-env (^jig system setup python | from json) }
export def workdir []: nothing -> string { ^jig system workdir python | str trim }
export def build []: nothing -> nothing { ^jig system phase python build }
export def install []: nothing -> nothing { ^jig system phase python install }
export def test []: nothing -> nothing { ^jig system phase python test }
