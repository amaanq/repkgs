# What `uses = [ "<name>" ]` means. jib's system/<name>.rs implements the phases and declares, in
# nix/schema.json, the default phase order, whether its packages are prebuilt, the platform it
# cannot serve and the options a package may set under `<name>.*`. This file binds the packages:
# `tool` (swappable per package as `<name>.tool`) and `tools` (a list, or spec -> list) go on PATH,
# `dependencies` add to the package's, `lock.<option> = fetcher` fills that option from the
# package's own lock file, and `stack` lists tools that are themselves built with the system.
# `sh` is for tools that spawn a shell by name (ninja, npm run, libtool).
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
  excludes =
    when:
    if when == "cross" then
      platform.cross
    else if when ? libc then
      platform.libc == when.libc
    else
      platform.libc != when.libcOtherThan;
  bindings = {
    make.tools = [ sh ];
    # make and bash come with baseTools (or the seed's for bootstrapTools packages)
    autotools.tools = [ sh ];
    cmake = {
      tool = buildPkgs.cmake;
      tools = [
        buildPkgs.ninja
        sh
      ];
    };
    vcxproj.tools = [ buildPkgs.ninja ];
    meson = {
      tool = buildPkgs.meson;
      tools = [
        buildPkgs.ninja
        sh
      ];
    };
    python = {
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
  };
in
builtins.mapAttrs (
  name: descriptor:
  let
    bound = bindings.${name} or { };
    options =
      lib.on (bound ? tool) {
        tool = {
          type = [ "set" ];
          default = bound.tool;
          doc = "the package that provides ${name}";
        };
      }
      // schema.options.${name};
    extra = if builtins.isFunction (bound.tools or [ ]) then bound.tools else _: bound.tools or [ ];
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
          extra = lib.on (bound ? tool) { tool.kind = "package"; };
        }
        // args
      );
    phases = map (verb: "${name}.${verb}") descriptor.phases;
    # what the package's `<name>` record starts from: every option's default, `lock` ones fetched
    defaults =
      if bound ? lock then
        source: optionDefaults // builtins.mapAttrs (_: f: f { inherit source; }) bound.lock
      else
        _: optionDefaults;
    tools = spec: lib.on (bound ? tool) [ spec.${name}.tool ] ++ extra spec;
    dependencies = bound.dependencies or [ ];
    prebuilt = descriptor.prebuilt or false;
    unsupported =
      if descriptor ? unsupported && excludes descriptor.unsupported.when then
        descriptor.unsupported.reason
      else
        null;
    stack = bound.stack or [ ];
  }
) schema.descriptors
