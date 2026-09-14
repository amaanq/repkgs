# go build / test / install from a GOPROXY tree or vendor/, jig phase go <verb>
export def --env setup []: nothing -> nothing { load-env (^jig system setup go | from json) }
export def workdir []: nothing -> string { ^jig system workdir go | str trim }
export def build []: nothing -> nothing { ^jig system phase go build }
export def test []: nothing -> nothing { ^jig system phase go test }
export def install []: nothing -> nothing { ^jig system phase go install }
