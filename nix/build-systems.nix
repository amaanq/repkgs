# What stands behind each `uses` name beyond schema.json's phases and options, its main tool (a
# recipe may swap it as `<name>.tool`), what else goes on PATH, runtime dependencies, lock-derived
# defaults and the stack built with it. Bare `sh` is on PATH where ninja, `npm run` or libtool
# spawn a shell by that name.
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
  cross = lib.on platform.cross;

  # bin scripts start `#!/usr/bin/env node`, and prebuilt .node addons (rollup's, esbuild's) need
  # libgcc_s.so.1 both while building and at run time
  node = [
    pkgs.nodejs
    pkgs.libgcc-shim
  ];
  beam = [ pkgs.erlang ];

  table = with buildPkgs; {
    make.tools = [ sh ];
    autotools.tools = [ sh ];
    cmake = {
      tool = cmake;
      tools = [
        ninja
        sh
      ];
    };
    vcxproj.tools = [ ninja ];
    meson = {
      tool = meson;
      tools = [
        ninja
        sh
      ];
    };
    python = {
      tool = cpython;
      # each member sees only those before it
      stack = [
        python-flit-core
        python-packaging
        python-pyproject-hooks
        python-build
        python-installer
      ];
      # extension modules link the target's libpython
      dependencies = cross [ pkgs.cpython ];
    };
    cargo = {
      tool = rust;
      # formatelf and git predate llvm and rust and swap in rust-bootstrap, whose std follows it
      tools = spec: cross [ pkgs."${spec.cargo.tool.pname}-std" ];
      lock.deps = fetch.cargoVendor;
    };
    cabal = {
      tool = cabal;
      tools = [ ghc-bootstrap ];
      plumbing = [ jape ];
      lock.deps = _: fetch.hackageSet { };
      # ghc-bignum puts -lgmp in every link, the RTS -lffi
      dependencies = [
        pkgs.gmp
        pkgs.libffi
      ];
    };
    luarocks = {
      tool = luarocks;
      tools = [ sh ];
      lock.deps = _: fetch.luaRocksSet { inherit lua; };
    };
    go = {
      tool = go;
      lock.deps = fetch.goModules;
    };
    pnpm = {
      tool = pnpm;
      tools = [
        nodejs
        sh
        libgcc-shim
      ];
      lock.deps = fetch.pnpmDeps;
      dependencies = node;
    };
    # uv.lock leaves build backends out, so an app gets every one the set has
    pyapp.tools = [
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
    ++ cross [ pkgs.rust-std ];
    pyapp.lock.deps = args: fetch.pythonDeps (args // { python = pkgs.cpython; });
    bundler = {
      tool = ruby;
      tools = [ sh ];
      lock.deps = fetch.gems;
    };
    deno = {
      tool = deno;
      lock.deps = fetch.denoDeps;
    };
    bun = {
      tool = bun;
      tools = [
        sh
        libgcc-shim
      ];
      lock.deps = fetch.bunDeps;
      dependencies = node;
    };
    yarn = {
      tool = yarn;
      tools = [
        nodejs
        sh
        libgcc-shim
      ];
      lock.deps = fetch.yarnDeps;
      dependencies = node;
    };
    # escripts start `#!/usr/bin/env escript` and releases exec erl
    mix = {
      tool = elixir;
      tools = [
        erlang
        hex
        rebar3
      ];
      lock.deps = fetch.hexDeps;
      dependencies = beam;
    };
    rebar3 = {
      tools = [
        rebar3
        erlang
      ];
      lock.deps = fetch.hexDeps;
      dependencies = beam;
    };
    npm = {
      tool = nodejs;
      tools = [
        sh
        libgcc-shim
      ];
      lock.deps = fetch.npmDeps;
      dependencies = node;
    };
  };

  refused =
    when:
    if when == "cross" then
      platform.cross
    else if when ? libc then
      platform.libc == when.libc
    else
      platform.libc != when.libcOtherThan;

  describe =
    name: descriptor:
    let
      entry = table.${name} or { };
      owned = entry ? tool;
      options =
        lib.on owned {
          tool = {
            type = [ "set" ];
            default = entry.tool;
            doc = "the package that provides ${name}";
          };
        }
        // schema.options.${name};
      extras = entry.tools or [ ];
    in
    {
      inherit options;
      validate =
        args:
        schema.validate (
          {
            system = name;
            extra = lib.on owned { tool.kind = "package"; };
          }
          // args
        );
      phases = map (phase: "${name}.${phase}") descriptor.phases;
      defaults =
        source:
        builtins.mapAttrs (_: option: option.default) options
        // builtins.mapAttrs (_: fetcher: fetcher { inherit source; }) (entry.lock or { });
      tools =
        spec:
        lib.on owned [ spec.${name}.tool ] ++ (if builtins.isFunction extras then extras spec else extras);
      dependencies = entry.dependencies or [ ];
      plumbing = entry.plumbing or [ ];
      prebuilt = descriptor.prebuilt or false;
      unsupported =
        if descriptor ? unsupported && refused descriptor.unsupported.when then
          descriptor.unsupported.reason
        else
          null;
      stack = entry.stack or [ ];
    };
in
builtins.mapAttrs describe schema.descriptors
