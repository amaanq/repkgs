# host-side daemon for jig's build cache. Run outside the sandbox: jigd $XDG_RUNTIME_DIR/jigd/socket
{ package, buildPkgs }:
package {
  name = "jigd";
  platforms.posix = true;
  version = "3.0.0";
  source = builtins.path {
    path = ../jig/src;
    name = "jigd-src";
    filter = path: _: builtins.baseNameOf path != "target";
  };
  uses = [ "cargo" ];
  cargo.tool = buildPkgs.rust-bootstrap;
  cargo.flags = [
    "-p"
    "jigd"
  ];
  tests.version = false; # a daemon, no --version
}
