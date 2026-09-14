# rebar3 with the locked Hex packages unpacked and compiled deps cached, jig phase rebar3 <verb>
export def --env setup []: nothing -> nothing { load-env (^jig system setup rebar3 | from json) }
export def workdir []: nothing -> string { ^jig system workdir rebar3 | str trim }
export def build []: nothing -> nothing { ^jig system phase rebar3 build }
export def test []: nothing -> nothing { ^jig system phase rebar3 test }
export def install []: nothing -> nothing { ^jig system phase rebar3 install }
