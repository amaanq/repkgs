# deno check, test, and lib/<name> with a launcher per entry, jig phase deno <verb>
export def --env setup []: nothing -> nothing { load-env (^jig system setup deno | from json) }
export def workdir []: nothing -> string { ^jig system workdir deno | str trim }
export def build []: nothing -> nothing { ^jig system phase deno build }
export def test []: nothing -> nothing { ^jig system phase deno test }
export def install []: nothing -> nothing { ^jig system phase deno install }
