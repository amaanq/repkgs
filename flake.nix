# nixbot builds .#checks (nix/checks.nix), .#packages is the whole set. The package set itself is
# default.nix and takes no inputs; nixpkgs is what treefmt and the seed build already use.
{
  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
  inputs.fenix = {
    url = "github:nix-community/fenix/078bcba335292f75f59987c29a0ea5a3edc5ec06";
    inputs.nixpkgs.follows = "nixpkgs";
  };

  outputs =
    { nixpkgs, fenix, ... }:
    let
      # aarch64-linux once its seed nar is uploaded (pkgs/se/seed/upload.nu)
      systems = [ "x86_64-linux" ];
      each = f: nixpkgs.lib.genAttrs systems f;
    in
    {
      packages = each (
        system:
        let
          bootstrap = import ./pkgs/se/seed/build.nix {
            inherit nixpkgs system;
            buildSystem = system;
          };
        in
        (import ./default.nix { inherit system; })
        // {
          nix = import ./nix/nix { pkgs = nixpkgs.legacyPackages.${system}; };
          bootstrap-llvm = bootstrap.llvm;
          bootstrap-rust-stage0 = bootstrap.stage0;
          bootstrap-rust = bootstrap.rust;
          bootstrap-nushell = bootstrap.nu;
          bootstrap-mrustc = nixpkgs.legacyPackages.${system}.mrustc-bootstrap.overrideAttrs {
            checkPhase = ''
              runHook preCheck
              run_rustc/output-1.90.0/prefix/bin/hello_world | grep -Fx 'Hello, world!'
              runHook postCheck
            '';
          };
          bootstrap-seed = bootstrap.seed;
        }
      );
      checks = each (system: import ./nix/checks.nix { inherit system nixpkgs; });
      formatter = each (system: import ./treefmt.nix { pkgs = nixpkgs.legacyPackages.${system}; });
      devShells = each (system: {
        default = import ./shell.nix { pkgs = nixpkgs.legacyPackages.${system}; };
        jigd =
          let
            pkgs = nixpkgs.legacyPackages.${system};
          in
          pkgs.mkShell {
            packages = [
              pkgs.cargo
              pkgs.rustc
              pkgs.clippy
              pkgs.taplo
              pkgs.nixfmt
              fenix.packages.${system}.latest.rustfmt
            ];
          };
      });
    };
}
