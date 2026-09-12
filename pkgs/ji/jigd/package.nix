# host-side daemon for jig's build cache. Run outside the sandbox: jigd $XDG_RUNTIME_DIR/jigd/socket
{ package, buildPkgs }:
package {
  name = "jigd";
  version = "3.0.0";
  source = builtins.path {
    path = ./src;
    name = "jigd-src";
    filter = path: _: builtins.baseNameOf path != "target";
  };
  uses = [ "cargo" ];
  cargo.tool = buildPkgs.rust-bootstrap;
  tests.version = false; # a daemon, no --version
}
