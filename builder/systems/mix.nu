# mix with the locked Hex packages unpacked and compiled deps cached, jig phase mix <verb>
export def --env setup []: nothing -> nothing { load-env (^jig system setup mix | from json) }
export def workdir []: nothing -> string { ^jig system workdir mix | str trim }
export def build []: nothing -> nothing { ^jig system phase mix build }
export def test []: nothing -> nothing { ^jig system phase mix test }
export def install []: nothing -> nothing { ^jig system phase mix install }
