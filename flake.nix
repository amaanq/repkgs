# CI builds .#checks (nix/checks.nix) and .#packages holds the whole set. The set is default.nix
# and takes no inputs, nixpkgs only serves the formatter, the shells and the seed build.
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
      # aarch64-linux joins once its seed nar is uploaded (pkgs/se/seed/upload.nu)
      forSystems = lib.genAttrs [ "x86_64-linux" ];

      seedOutputs =
        system:
        let
          seed = import ./pkgs/se/seed {
            inherit system;
            buildSystem = system;
          };
          stages = lib.mapAttrs' (
            version: lib.nameValuePair "bootstrap-rust-${builtins.replaceStrings [ "." ] [ "_" ] version}"
          ) seed.rustStages;
        in
        {
          bootstrap-llvm = seed.llvm;
          bootstrap-jig = seed.jig;
          bootstrap-jib = seed.jib;
          bootstrap-rust-stage0 = seed.stage0;
          bootstrap-rust = seed.rust;
          bootstrap-nushell = seed.nu;
          bootstrap-mrustc = seed.mrustc;
          bootstrap-seed = seed.seed;
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
        // seedOutputs system
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
