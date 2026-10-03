{
  pkgs,
  ps,
  llvm,
  source,
}:
let
  inherit (pkgs) lib;
  buildRust = import ./rust-build.nix { inherit pkgs ps llvm; };
  releases = (fromTOML (builtins.readFile ./rust-chain.toml)).release;
  mrustc = pkgs.mrustc-bootstrap.overrideAttrs {
    checkPhase = ''
      runHook preCheck
      run_rustc/output-1.90.0/prefix/bin/hello_world | grep -Fx 'Hello, world!'
      runHook postCheck
    '';
  };
  chain =
    lib.foldl'
      (
        previous: release:
        let
          compiler = buildRust {
            inherit (release) version;
            src = pkgs.fetchurl {
              url = "https://static.rust-lang.org/dist/rustc-${release.version}-src.tar.gz";
              inherit (release) hash;
            };
            stage0 = previous.compiler;
            stage = if previous.stages == { } then 2 else 1;
          };
        in
        {
          inherit compiler;
          stages = previous.stages // {
            ${release.version} = compiler;
          };
        }
      )
      {
        compiler = mrustc;
        stages = { };
      }
      releases;
  rustSource = source "rust";
  rust = buildRust {
    inherit (rustSource) version;
    src = rustSource.default;
    stage0 = chain.compiler;
    final = true;
    backend = null;
  };
in
{
  inherit mrustc rust;
  stage0 = chain.compiler;
  rustStages = chain.stages;
}
