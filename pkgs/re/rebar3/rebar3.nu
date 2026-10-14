use core.nu [act dep cue launcher]

export def build []: nothing -> nothing {
   let c = (act)
   with-env {HOME: $c.build, REBAR_OFFLINE: "1"} { cue escript bootstrap }
   let escript = $"($c.out)/lib/escript/rebar3"
   mkdir ($escript | path dirname)
   cp _build/prod/bin/rebar3 $escript
   let erlang = (dep erlang "escripts run under the target erlang")
   launcher rebar3 $"($erlang)/bin/escript" [$escript]
}
