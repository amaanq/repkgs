# cabal v2-build against the set's hackage repository with units cached per unit id, jig phase cabal <verb>
export def --env setup []: nothing -> nothing { load-env (^jig system setup cabal | from json) }
export def workdir []: nothing -> string { ^jig system workdir cabal | str trim }
export def build []: nothing -> nothing { ^jig system phase cabal build }
export def test []: nothing -> nothing { ^jig system phase cabal test }
export def install []: nothing -> nothing { ^jig system phase cabal install }
