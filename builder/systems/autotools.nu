# autoconf configure, then make, jig phase autotools <verb>
export def --env setup []: nothing -> nothing { load-env (^jig system setup autotools | from json) }
export def workdir     []: nothing -> string  { ^jig system workdir autotools | str trim }
export def configure   []: nothing -> nothing { ^jig system phase   autotools configure }
export def build       []: nothing -> nothing { ^jig system phase   autotools build }
export def test        []: nothing -> nothing { ^jig system phase   autotools test }
export def install     []: nothing -> nothing { ^jig system phase   autotools install }
