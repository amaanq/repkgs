{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    rom = {
      url = "github:manic-systems/rom/refactor";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    fenix = {
      url = "github:nix-community/fenix/078bcba335292f75f59987c29a0ea5a3edc5ec06";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  # The set itself needs none of these inputs, nixpkgs only builds the formatter, the shells and
  # the peanut. aarch64-linux joins once its peanut is uploaded.
  outputs =
    {
      nixpkgs,
      fenix,
      rom,
      ...
    }:
    let
      each = nixpkgs.lib.genAttrs [ "x86_64-linux" ];
    in
    {
      packages = each (
        system:
        let
          peanut = import ./pkgs/pe/peanut {
            inherit system;
            buildSystem = system;
          };
        in
        import ./default.nix { inherit system; }
        // {
          nix = import ./nix/nix { pkgs = nixpkgs.legacyPackages.${system}; };
          bootstrap-llvm = peanut.llvm;
          bootstrap-mummer = peanut.mummer;
          bootstrap-bigtop = peanut.bigtop;
          bootstrap-rust = peanut.rust;
          bootstrap-nushell = peanut.nu;
          bootstrap-mrustc = peanut.mrustc;
          bootstrap-peanut = peanut.peanut;
        }
      );

      checks = each (system: import ./nix/checks.nix { inherit system nixpkgs; });

      nixosModules.mummerd = ./nix/module.nix;
      nixosModules.ringmasterd = ./nix/ringmasterd.nix;

      formatter = each (system: import ./nix/format.nix { pkgs = nixpkgs.legacyPackages.${system}; });

      devShells = each (
        system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
          host = pkgs.stdenv.hostPlatform;
        in
        {
          default = import ./shell.nix {
            inherit pkgs;
            rom = rom.packages.${system}.default;
          };
          rust = pkgs.mkShell {
            # the wrapped clang adds flags the bpf target rejects
            RINGMASTERD_CLANG = pkgs.lib.getExe' pkgs.llvmPackages.clang-unwrapped "clang";
            packages =
              with pkgs;
              [
                cargo
                rustc
                clippy
                taplo
                nixfmt
                fenix.packages.${host.system}.latest.rustfmt
              ]
              ++ lib.optionals (host.isLinux && (host.isx86_64 || host.isAarch64)) [
                wild
                clang
              ];
          };
        }
      );
    };
}
