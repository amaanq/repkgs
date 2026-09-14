# luarocks make into $out as the rocks tree, jig phase luarocks install
export def setup []: nothing -> nothing { }
export def workdir []: nothing -> string { ^jig system workdir luarocks | str trim }
export def install []: nothing -> nothing { ^jig system phase luarocks install }
