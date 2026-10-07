{
  pkgs,
  ps,
  llvm,
  source,
}:
let
  buildRust = import ./rust-build.nix { inherit pkgs ps llvm; };
  mrustc = import ./mrustc.nix { inherit pkgs; };
  rustSource = source "rust";
  rust = buildRust {
    inherit (rustSource) version;
    src = rustSource.default;
    stage0 = mrustc;
    final = true;
    backend = null;
  };
in
{
  inherit mrustc rust;
}
