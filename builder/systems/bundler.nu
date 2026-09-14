# a Ruby application installed with bundler into lib/<name> with bin/ stubs, jig phase bundler <verb>
export def --env setup []: nothing -> nothing { load-env (^jig system setup bundler | from json) }
export def workdir []: nothing -> string { ^jig system workdir bundler | str trim }
export def build []: nothing -> nothing { ^jig system phase bundler build }
export def test []: nothing -> nothing { ^jig system phase bundler test }
export def install []: nothing -> nothing { ^jig system phase bundler install }
