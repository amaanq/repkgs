# cmake configure / build / ctest / install with Ninja, jig phase cmake <verb>
export def --env setup []: nothing -> nothing { load-env (^jig system setup cmake | from json) }
export def workdir []: nothing -> string { ^jig system workdir cmake | str trim }
export def configure []: nothing -> nothing { ^jig system phase cmake configure }
export def build []: nothing -> nothing { ^jig system phase cmake build }
export def test []: nothing -> nothing { ^jig system phase cmake test }
export def install []: nothing -> nothing { ^jig system phase cmake install }
