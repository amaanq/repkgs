# What `uses = [ "<name>" ]` means: jib's system/<name>.rs implements the phases, `phases` is the default
# order (build test install unless said otherwise), `tool` (swappable per package as `<name>.tool`)
# and `tools` (a list, or spec -> list) go on PATH, `dependencies` add to
# the package's, `prebuilt` is the package's default for that field, `unsupported` is a reason
# (or null) why no user of it can build on this platform, and `options` (emitted by `jib schema`
# into nix/schema.json) are what a
# package may set under `<name>.*`: `{ type; default; doc; }` each, `type` the `builtins.typeOf`
# names allowed, checked at eval time along with the name. The system reads the merged result
# from the spec under `<name>`. Every system has `root`; `deps` (the fetched tree of locked dependencies,
# `lock.deps = fetcher` defaults it to the package's own lock file) and `flags` (extra words on
# the tool's command line) mean the same wherever they exist. `repkgs options` renders this as a
# table. `sh` is for tools that spawn a shell by name (ninja, npm run, libtool).
{
  buildPkgs,
  pkgs,
  platform,
  fetch,
  sh,
  lib,
}:
let
  schema = import ./schema.nix;
  # bin scripts say #!/usr/bin/env node, prebuilt .node addons (rollup, esbuild) link libgcc_s.so.1
  nodeDeps = [
    pkgs.nodejs
    pkgs.libgcc-shim
  ];
  nodeTools = [ buildPkgs.libgcc-shim ]; # the same addons while building (builder/node-common.nu)
in
builtins.mapAttrs
  (
    name: bs:
    let
      options =
        lib.on (bs ? tool) {
          tool = {
            type = [ "set" ];
            default = bs.tool;
            doc = "the package that provides ${name}";
          };
        }
        // schema.options.${name};
      extra = if builtins.isFunction (bs.tools or [ ]) then bs.tools else _: bs.tools or [ ];
      # one set per build system, only `lock` options depend on the package
      optionDefaults = builtins.mapAttrs (_: o: o.default) options;
    in
    {
      inherit options;
      validate =
        args:
        schema.validate (
          {
            system = name;
            extra = if bs ? tool then { tool.kind = "package"; } else { };
          }
          // args
        );
      phases = map (v: "${name}.${v}") (
        bs.phases or [
          "build"
          "test"
          "install"
        ]
      );
      phase = builtins.listToAttrs (
        map
          (verb: {
            name = "${name}.${verb}";
            value = {
              test = verb == "test";
            };
          })
          [
            "configure"
            "build"
            "test"
            "install"
          ]
      );
      # what the package's `<name>` record starts from: every option's default, `lock` ones fetched
      defaults =
        if bs ? lock then
          source: optionDefaults // builtins.mapAttrs (_: f: f { inherit source; }) bs.lock
        else
          _: optionDefaults;
      # `tool`: the build system's own program, a package may swap it (`cmake.tool = …`)
      tools = spec: (if bs ? tool then [ spec.${name}.tool ] else [ ]) ++ extra spec;
      dependencies = bs.dependencies or [ ];
      prebuilt = bs.prebuilt or false;
      unsupported = bs.unsupported or null;
      stack = bs.stack or [ ];
    }
  )
  {
    make = {
      phases = [
        "configure"
        "build"
        "test"
        "install"
      ];
      tools = [ sh ];
    };
    autotools = {
      unsupported =
        if platform.libc == "msvc" then
          "configure and libtool do not know the MSVC ABI, config.sub rejects the triple"
        else
          null;
      phases = [
        "configure"
        "build"
        "test"
        "install"
      ];
      # make and bash come with baseTools (or the seed's for bootstrapTools packages)
      tools = [ sh ];
    };
    cmake = {
      phases = [
        "configure"
        "build"
        "test"
        "install"
      ];
      tool = buildPkgs.cmake;
      tools = [
        buildPkgs.ninja
        sh
      ];
    };
    vcxproj = {
      unsupported =
        if platform.libc != "msvc" then "Visual Studio projects describe an MSVC-ABI build" else null;
      phases = [
        "configure"
        "build"
        "install"
      ];
      tools = [ buildPkgs.ninja ];
    };
    meson = {
      phases = [
        "configure"
        "build"
        "test"
        "install"
      ];
      tool = buildPkgs.meson;
      tools = [
        buildPkgs.ninja
        sh
      ];
    };
    python = {
      phases = [
        "build"
        "install"
        "test"
      ]; # tests import the installed module
      # the PEP 517 front end and its deps. Members of the stack itself get only what exists before them
      tool = buildPkgs.cpython;
      stack = with buildPkgs; [
        python-flit-core
        python-packaging
        python-pyproject-hooks
        python-build
        python-installer
      ];
    };
    cargo = {
      # `cargo.tool = buildPkgs.rust-bootstrap` for what must exist before llvm and rust are
      # built (formatelf, git). Cross: std for the target is its own package, <tool>-std
      tool = buildPkgs.rust;
      tools = spec: lib.on platform.cross [ pkgs."${spec.cargo.tool.pname}-std" ];
      lock.deps = fetch.cargoVendor;
    };
    cabal = {
      unsupported = if platform.cross then "ghc-bootstrap only targets the build machine" else null;
      # `cabal.tool = buildPkgs.cabal-bootstrap` for cabal itself
      tool = buildPkgs.cabal;
      tools = [
        buildPkgs.ghc-bootstrap
        buildPkgs.jsem
      ];
      dependencies = [
        pkgs.gmp # ghc-bignum: every linked program wants -lgmp
        pkgs.libffi # and the RTS -lffi
      ];
      lock.deps = _: fetch.hackageSet { };
    };
    luarocks = {
      phases = [ "install" ]; # luarocks make builds into --tree
      tool = buildPkgs.luarocks;
      tools = [ sh ];
      lock.deps = _: fetch.luaRocksSet { inherit (buildPkgs) lua; };
    };
    go = {
      tool = buildPkgs.go;
      lock.deps = fetch.goModules;
    };
    pnpm = {
      tool = buildPkgs.pnpm;
      tools = [
        buildPkgs.nodejs
        sh
      ]
      ++ nodeTools;
      lock.deps = fetch.pnpmDeps;
      dependencies = nodeDeps;
    };
    pyapp = {
      # binary wheels carry upstream-linked .so files: finish implants interp/RUNPATH like for any
      # prebuilt package (after split-debug, llvm-objcopy crashes on formatelf's layout)
      prebuilt = true;
      lock.deps = args: fetch.pythonDeps (args // { python = pkgs.cpython; });
      # uv.lock does not lock build backends: every one the set has
      tools =
        with buildPkgs;
        [
          cpython
          python-build
          python-installer
          python-pyproject-hooks
          python-packaging
          python-flit-core
          python-setuptools
          python-setuptools-scm
          python-hatchling
          python-hatch-vcs
          python-cython
          maturin
          rust
        ]
        ++ lib.on platform.cross [ pkgs.rust-std ];
    };
    bundler = {
      tool = buildPkgs.ruby;
      tools = [ sh ];
      lock.deps = fetch.gems;
    };
    deno = {
      tool = buildPkgs.deno;
      lock.deps = fetch.denoDeps;
    };
    bun = {
      tool = buildPkgs.bun;
      tools = [ sh ] ++ nodeTools;
      lock.deps = fetch.bunDeps;
      dependencies = nodeDeps;
    };
    yarn = {
      tool = buildPkgs.yarn;
      tools = [
        buildPkgs.nodejs
        sh
      ]
      ++ nodeTools;
      lock.deps = fetch.yarnDeps;
      dependencies = nodeDeps;
    };
    mix = {
      # no test phase by default: MIX_ENV=test deps are outside the prod lock subset
      phases = [
        "build"
        "install"
      ];
      tool = buildPkgs.elixir;
      tools = [
        buildPkgs.erlang
        buildPkgs.hex
        buildPkgs.rebar3
      ];
      lock.deps = fetch.hexDeps;
      dependencies = [ pkgs.erlang ]; # escripts say #!/usr/bin/env escript, releases exec erl
    };
    rebar3 = {
      # no test phase by default: eunit/ct deps live in the test profile, outside rebar.lock
      phases = [
        "build"
        "install"
      ];
      tools = [
        buildPkgs.rebar3
        buildPkgs.erlang
      ];
      lock.deps = fetch.hexDeps;
      dependencies = [ pkgs.erlang ];
    };
    npm = {
      tool = buildPkgs.nodejs;
      tools = [ sh ] ++ nodeTools;
      lock.deps = fetch.npmDeps;
      dependencies = nodeDeps;
    };
  }
