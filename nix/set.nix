# The package set for one platform and what it is made from: { pkgs, bootstrap, buildSystems }.
# default.nix is `pkgs` alone; nix/checks.nix reads the rest.
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
  editing = import ./overrides.nix;
  overrideTree = editing.merge overrides;
  bootstrap = import ../bootstrap { inherit seed system; };

  givenTools = builtins.mapAttrs (
    _: tool: if builtins.isString tool || builtins.isPath tool then builtins.storePath tool else tool
  ) buildTools;
  tools = {
    inherit (bootstrap) jig jib nu;
  }
  // givenTools;
  unknownTools = builtins.attrNames (
    removeAttrs buildTools [
      "jig"
      "jib"
      "nu"
    ]
  );

  target = (import ./platforms.nix).byName.${platform} or (throw "no platform ${platform}");
  stage = bootstrap.toolchain.${platform} (
    on (target.libc == "msvc") {
      sdk = fetch.windowsSdk {
        manifest = (readSources ../pkgs/wi/windows-sdk/sources.toml).fetch "default";
        arch = target.cpu;
      };
    }
  );
  cross = platform != system;
  plat = stage.platform // {
    inherit system cross;
    buildRustTriple = bootstrap.stage1.platform.rustTriple;
    inherit (bootstrap) buildTriple;
    # its address cap is $QEMU_RESERVED_VA (jib prepare), build systems want one word here
    emulator =
      if cross && target.os == "linux" then
        [ "${buildPkgs.qemu}/bin/qemu-${stage.platform.names.qemu}" ]
      else if cross && target.os == "windows" && target.cpu == "x86_64" then
        [ "${buildPkgs.wine}/bin/wine" ]
      else
        [ ];
  };
  toolchain = stage.cc;

  rust = import ./set/rust.nix {
    inherit rustToolchain;
    triple = plat.buildRustTriple;
  };
  buildPkgs =
    (
      if cross then
        (import ./set.nix {
          platform = system;
          inherit
            seed
            rustToolchain
            buildTools
            system
            overrides
            packages
            ;
        }).pkgs
      else
        self
    )
    // rust.packages;

  buildSystems = import ./build-systems.nix {
    inherit buildPkgs fetch lib;
    platform = plat;
    pkgs = self;
    # build systems that spawn `sh` by name get the seed's dash
    sh = bootstrap.seed;
  };
  # On PATH after the toolchain and the build systems' tools. GNU userland precedes the seed because
  # build scripts in the wild need more than toybox. llvm brings the object tools, the seed bsdtar,
  # sh. `bootstrap` is what the base userland itself is built with (`bootstrapTools = true`).
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

  package = import ./package {
    platform = plat;
    inherit
      lib
      toolchain
      buildSystems
      baseTools
      ;
    # `prebuilt = true` patches upstream ELFs with it (builder/implant.nu)
    relocTools = [ buildPkgs.formatelf ];
    inherit (tools) jib nu;
    inherit (bootstrap) seed;
    launch = stage.launch or null;
    dlaudit = stage.dlaudit or null;
    # identity when there are none, so the common case allocates nothing per package
    edit = if overrideTree == { } then null else editing.apply self overrideTree;
    # [pin] sys names resolve here
    pkgs = self;
  };

  fetch = import ./fetch.nix {
    jig = givenTools.jig or bootstrap.stage0.jig;
    sevenzip = buildPkgs."7zip";
    inherit (bootstrap) seed;
    inherit system;
    inherit (plat) cpu;
  };
  readSources = import ./sources.nix {
    unpacker = bootstrap.seed;
    inherit (tools) jig;
    inherit system;
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
    platform = plat;
    pkgs = self;
    # cpython's package.nix adds .env/.project with this (nix/python.nix)
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
  unknownOverrides = editing.unknown (self // found.aliases) overrideTree;
in
{
  # attrNames alone would not force the platform: `list --for typo` showed this machine's set
  pkgs = builtins.seq stage (
    if unknownTools != [ ] then
      throw "unknown build tools ${toString unknownTools}"
    else if rustToolchain != null && cross then
      throw "rustToolchain currently supports native builds only, omit it for ${platform}"
    else if rustToolchain != null && rust.missing != [ ] then
      throw "rustToolchain ${toString rust.path} is missing ${toString rust.missing}"
    else if unknownOverrides != [ ] then
      throw "overrides: no packages named ${toString unknownOverrides}"
    else
      self
  );
  inherit
    bootstrap
    buildSystems
    toolchain
    tools
    ;
  bundle = import ./bundle.nix { inherit (tools) jib; };
}
