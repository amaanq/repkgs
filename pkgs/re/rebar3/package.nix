# rebar3 bootstraps itself offline from its vendored dependencies into one escript
{
  package,
  pkgs,
  buildPkgs,
}:
package {
  name = "rebar3";
  buildDependencies = [ buildPkgs.erlang ];
  dependencies = [ pkgs.erlang ];
  phases = [
    {
      name = "bootstrap";
      run = ''
        with-env {HOME: $c.build, REBAR_OFFLINE: "1"} { x escript bootstrap }
        mkdir $"($c.out)/lib/escript"
        cp _build/prod/bin/rebar3 $"($c.out)/lib/escript/rebar3"
        write-launcher rebar3 $"(dep-root erlang "escripts run under the target erlang")/bin/escript" [$"($c.out)/lib/escript/rebar3"]
      '';
    }
  ];
}
