# CI builds .#checks (nix/checks.nix) and .#packages holds the whole set. The set is default.nix
# and takes no inputs, nixpkgs only serves the formatter, the shells and the peanut build.
{
  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
  inputs.rom = {
    url = "github:manic-systems/rom/refactor";
    inputs.nixpkgs.follows = "nixpkgs";
  };
  inputs.fenix = {
    url = "github:nix-community/fenix/078bcba335292f75f59987c29a0ea5a3edc5ec06";
    inputs.nixpkgs.follows = "nixpkgs";
  };

  outputs =
    {
      nixpkgs,
      fenix,
      rom,
      ...
    }:
    let
      inherit (nixpkgs) lib;
      # aarch64-linux joins once its peanut nar is uploaded (pkgs/pe/peanut/upload.nu)
      forSystems = lib.genAttrs [ "x86_64-linux" ];

      peanutOutputs =
        system:
        let
          peanut = import ./pkgs/pe/peanut {
            inherit system;
            buildSystem = system;
          };
          stages = lib.mapAttrs' (
            version: lib.nameValuePair "bootstrap-rust-${builtins.replaceStrings [ "." ] [ "_" ] version}"
          ) peanut.rustStages;
        in
        {
          bootstrap-llvm = peanut.llvm;
          bootstrap-mummer = peanut.mummer;
          bootstrap-bigtop = peanut.bigtop;
          bootstrap-rust-stage0 = peanut.stage0;
          bootstrap-rust = peanut.rust;
          bootstrap-nushell = peanut.nu;
          bootstrap-mrustc = peanut.mrustc;
          bootstrap-peanut = peanut.peanut;
        }
        // stages;

      rustShell =
        pkgs:
        let
          host = pkgs.stdenv.hostPlatform;
          wild = host.isLinux && (host.isx86_64 || host.isAarch64);
        in
        pkgs.mkShell {
          packages =
            (with pkgs; [
              cargo
              rustc
              clippy
              taplo
              nixfmt
            ])
            ++ [ fenix.packages.${host.system}.latest.rustfmt ]
            ++ lib.optionals wild [
              pkgs.wild
              pkgs.clang
            ];
        };
    in
    {
      packages = forSystems (
        system:
        import ./default.nix { inherit system; }
        // {
          nix = import ./nix/nix { pkgs = nixpkgs.legacyPackages.${system}; };
        }
        // peanutOutputs system
      );
      checks = forSystems (system: import ./nix/checks.nix { inherit system nixpkgs; });
      formatter = forSystems (
        system: import ./nix/format.nix { pkgs = nixpkgs.legacyPackages.${system}; }
      );
      devShells = forSystems (
        system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
        in
        {
          default = import ./shell.nix {
            inherit pkgs;
            rom = rom.packages.${system}.default;
          };
          rust = rustShell pkgs;
        }
      );
    };
}
