# plain Makefile projects, jig phase make <verb>
export def --env setup []: nothing -> nothing { load-env (^jig system setup make | from json) }
export def workdir []: nothing -> string { ^jig system workdir make | str trim }
export def configure []: nothing -> nothing { ^jig system phase make configure }
export def build []: nothing -> nothing { ^jig system phase make build }
export def test []: nothing -> nothing { ^jig system phase make test }
export def install []: nothing -> nothing { ^jig system phase make install }
