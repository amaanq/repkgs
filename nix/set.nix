# One platform's set, `pkgs`, beside the parts it is assembled from (bootstrap, toolchain, tools,
# buildSystems, bundle). default.nix hands out `pkgs` and nix/checks.nix and the tooling read the
# rest.
{
  system ? builtins.currentSystem,
  platform ? system,
  seed ? null,
  rustToolchain ? null,
  buildTools ? { },
  overrides ? { },
  packages ? { },
  features ? { },
}:
let
  lib = import ./lib.nix;
  inherit (lib) on;
  bootstrap = import ../bootstrap { inherit seed system; };
  editing = import ./overrides.nix;
  overrideTree = editing.merge overrides;

  # shim, hoist and nu may come prebuilt as store paths, a development shortcut
  given = builtins.mapAttrs (
    _: tool: if builtins.isString tool || builtins.isPath tool then builtins.storePath tool else tool
  ) buildTools;
  strayTools = builtins.attrNames (
    removeAttrs buildTools [
      "shim"
      "hoist"
      "nu"
    ]
  );
  tools = {
    inherit (bootstrap) shim hoist nu;
  }
  // given;

  cross = platform != system;
  target = (import ./platforms.nix).byName.${platform} or (throw "no platform ${platform}");
  stage = bootstrap.toolchain.${platform} (
    on (target.libc == "msvc") {
      sdk = fetch.windowsSdk {
        manifest = (readSources ../pkgs/wi/windows-sdk/recipe.toml).fetch "default";
        arch = target.cpu;
      };
    }
  );
  toolchain = stage.cc;
  # build systems want one word for the emulator, and hoist prepare caps its address space with
  # $QEMU_RESERVED_VA
  emulator =
    if cross && target.os == "linux" then
      [ "${buildPkgs.qemu}/bin/qemu-${stage.platform.names.qemu}" ]
    else if cross && target.os == "windows" && target.cpu == "x86_64" then
      [ "${buildPkgs.wine}/bin/wine" ]
    else
      [ ];
  facts = stage.platform // {
    inherit system cross emulator;
    inherit (bootstrap) buildTriple;
    buildRustTriple = bootstrap.stage1.platform.rustTriple;
  };

  rust = import ./set/rust.nix {
    inherit rustToolchain;
    triple = facts.buildRustTriple;
  };
  buildPkgs =
    (
      if cross then
        (import ./set.nix {
          platform = system;
          inherit
            system
            seed
            rustToolchain
            buildTools
            overrides
            packages
            ;
        }).pkgs
      else
        self
    )
    // rust.packages;

  # after the toolchain and the build systems' tools come GNU tools, which build scripts in the
  # wild need where toybox falls short, then llvm's object tools and the seed's bsdtar and sh.
  # `bootstrapTools = true` packages, the base tools among them, get only the latter
  seedTools = [
    bootstrap.llvm
    bootstrap.seed
  ];
  baseTools = {
    bootstrap = seedTools;
    full =
      (with buildPkgs; [
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
      ])
      ++ seedTools;
  };

  readSources = import ./sources.nix {
    unpacker = bootstrap.seed;
    inherit (tools) shim;
    inherit system;
  };
  fetch = import ./fetch.nix {
    shim = given.shim or bootstrap.stage0.shim;
    sevenzip = buildPkgs."7zip";
    inherit (bootstrap) seed;
    inherit system;
    inherit (facts) cpu;
  };
  buildSystems = import ./build-systems.nix {
    inherit buildPkgs fetch lib;
    platform = facts;
    pkgs = self;
    # the seed's dash, for tools that start `sh` by name
    sh = bootstrap.seed;
  };
  package = import ./package {
    platform = facts;
    inherit
      lib
      toolchain
      buildSystems
      baseTools
      ;
    inherit (tools) hoist nu;
    inherit (bootstrap) seed;
    launch = stage.launch or null;
    dlaudit = stage.dlaudit or null;
    # `prebuilt = true` patches upstream ELFs with it (builder/implant.nu)
    relocTools = [ buildPkgs.formatelf ];
    # null without overrides, so no package pays for an identity edit
    edit = if overrideTree == { } then null else editing.apply self overrideTree;
    pkgs = self;
  };

  # what a package.nix or recipe.toml may ask for
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
    pkgs = self;
    # for `helpers = "python"` (nix/package/manifest.nix)
    pythonProject =
      package: python:
      import ./python.nix {
        inherit
          package
          fetch
          python
          buildPkgs
          ;
        pkgs = self;
      };
  };
  found = import ./set/packages.nix {
    inherit packages self;
    call = import ./set/call.nix {
      inherit
        scope
        package
        readSources
        overrideTree
        features
        self
        ;
      overrides = editing;
    };
  };
  self = found.all;
  strayOverrides = editing.unknown (self // found.aliases) overrideTree;
in
{
  # forcing `stage` makes a misspelt platform fail even when only attrNames are asked for
  pkgs = builtins.seq stage (
    assert lib.check "manicpkgs" [
      (on (strayTools != [ ]) [ "unknown build tools ${toString strayTools}" ])
      (on (rustToolchain != null && cross) [
        "rustToolchain currently supports native builds only, omit it for ${platform}"
      ])
      (on (rustToolchain != null && !cross && rust.missing != [ ]) [
        "rustToolchain ${toString rust.path} is missing ${toString rust.missing}"
      ])
      (on (strayOverrides != [ ]) [ "overrides: no packages named ${toString strayOverrides}" ])
    ];
    self
  );
  inherit
    bootstrap
    buildSystems
    toolchain
    tools
    ;
  bundle = import ./bundle.nix { inherit (tools) hoist; };
}
