use core.nu [ctx dep-root x write-launcher]

export def build []: nothing -> nothing {
   let c = (ctx)
   with-env {HOME: $c.build, REBAR_OFFLINE: "1"} { x escript bootstrap }
   let escript = $"($c.out)/lib/escript/rebar3"
   mkdir ($escript | path dirname)
   cp _build/prod/bin/rebar3 $escript
   let erlang = (dep-root erlang "escripts run under the target erlang")
   write-launcher rebar3 $"($erlang)/bin/escript" [$escript]
}
