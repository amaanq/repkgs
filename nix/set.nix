# One platform's package set and the parts it is built from. default.nix hands out `pkgs`, the
# checks and tooling read the rest.
{
  system ? builtins.currentSystem,
  platform ? system,
  peanut ? null,
  rustToolchain ? null,
  buildTools ? { },
  overrides ? { },
  packages ? { },
  features ? { },
}:
let
  lib = import ./lib.nix;
  inherit (lib) on;
  overriding = import ./overrides.nix;
  boot = import ../bootstrap { inherit peanut system; };

  tree = overriding.merge overrides;
  cross = platform != system;
  words = builtins.concatStringsSep " ";

  # mummer, bigtop and nu may arrive prebuilt as store paths while working on them
  given = builtins.mapAttrs (
    _: tool: if builtins.isString tool || builtins.isPath tool then builtins.storePath tool else tool
  ) buildTools;
  strays = builtins.filter (
    name:
    !builtins.elem name [
      "mummer"
      "bigtop"
      "nu"
    ]
  ) (builtins.attrNames buildTools);
  tools = {
    inherit (boot) mummer bigtop nu;
  }
  // given;

  target = (import ./platforms.nix).byName.${platform} or (throw "no platform ${platform}");
  stage = boot.toolchain.${target.name} (
    on (target.libc == "msvc") {
      sdk = fetch.windowsSdk {
        manifest = (readSources ../pkgs/wi/windows-sdk).fetch "default";
        arch = target.cpu;
      };
    }
  );

  # one word, since build systems take the emulator as one, and bigtop's prepare caps its address
  # space through QEMU_RESERVED_VA
  emulator =
    if cross && target.os == "linux" then
      [ "${buildPkgs.qemu}/bin/qemu-${stage.platform.names.qemu}" ]
    else if cross && target.os == "windows" && target.cpu == "x86_64" then
      [ "${buildPkgs.wine}/bin/wine" ]
    else
      [ ];
  facts = stage.platform // {
    inherit system cross emulator;
    inherit (boot) buildTriple;
    buildRustTriple = boot.stage1.platform.rustTriple;
  };

  rust = import ./set/rust.nix {
    inherit rustToolchain;
    triple = facts.buildRustTriple;
  };
  # the build machine's set never sees `features`
  buildPkgs =
    (
      if cross then
        (import ./set.nix {
          inherit
            system
            peanut
            rustToolchain
            buildTools
            overrides
            packages
            ;
          platform = system;
        }).pkgs
      else
        all
    )
    // rust.packages;

  # GNU tools for build scripts where toybox falls short, then llvm's object tools and the
  # peanut's bsdtar and sh. `bootstrapTools = true` packages get only the last two.
  baseTools = rec {
    bootstrap = [
      boot.llvm
      boot.peanut
    ];
    full =
      with buildPkgs;
      [
        coreutils
        sed
        grep
        gawk
        diffutils
        findutils
        patch
        gnumake
        bash
        pkgconf
      ]
      ++ bootstrap;
  };

  readSources = import ./sources.nix {
    unpacker = boot.peanut;
    inherit (tools) mummer;
    inherit system;
  };
  fetch = import ./fetch.nix {
    mummer = given.mummer or boot.stage0.mummer;
    sevenzip = buildPkgs."7zip";
    inherit (boot) peanut;
    inherit system;
    inherit (facts) cpu;
  };
  buildSystems = import ./build-systems.nix {
    inherit buildPkgs fetch lib;
    platform = facts;
    pkgs = all;
    sh = boot.peanut;
  };
  toolchain = stage.cc;
  package = import ./package {
    platform = facts;
    inherit
      lib
      toolchain
      buildSystems
      baseTools
      ;
    inherit (tools) bigtop nu;
    inherit (boot) peanut;
    launch = stage.launch or null;
    dlaudit = stage.dlaudit or null;
    # `prebuilt = true` packages get upstream ELFs patched with it
    relocTools = [ buildPkgs.formatelf ];
    edit = if tree == { } then null else overriding.apply all tree;
    pkgs = all;
  };

  scope = {
    inherit
      package
      fetch
      toolchain
      buildPkgs
      lib
      on
      ;
    platform = facts;
    pkgs = all;
    pythonProject =
      wrap: python:
      import ./python.nix {
        package = wrap;
        inherit fetch python buildPkgs;
        pkgs = all;
      };
  };
  found = import ./set/packages.nix {
    inherit packages;
    self = all;
    call = import ./set/call.nix {
      inherit
        scope
        package
        features
        readSources
        ;
      overrideTree = tree;
      self = all;
      overrides = overriding;
    };
  };
  inherit (found) all;
in
{
  # forced first so a misspelt platform fails even when only names are asked for
  pkgs = builtins.seq stage (
    assert lib.check "motley" [
      (on (strays != [ ]) [ "unknown build tools ${words strays}" ])
      (on (rustToolchain != null && cross) [
        "rustToolchain currently supports native builds only, omit it for ${platform}"
      ])
      (on (rustToolchain != null && !cross && rust.missing != [ ]) [
        "rustToolchain ${toString rust.path} is missing ${words rust.missing}"
      ])
      (
        let
          unknown = overriding.unknown (all // found.aliases) tree;
        in
        on (unknown != [ ]) [ "overrides: no packages named ${words unknown}" ]
      )
    ];
    all
  );
  bootstrap = boot;
  inherit buildSystems toolchain tools;
  bundle = import ./bundle.nix { inherit (tools) bigtop; };
}
