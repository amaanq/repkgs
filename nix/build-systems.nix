# The packages behind each `uses` entry. hoist's system/<name>.rs owns the phases and declares in
# nix/schema.json their default order, whether its packages are prebuilt, the platforms it cannot
# serve and the `<name>.*` options. Here a system gets its `tool` (a package may swap it through
# `<name>.tool`) and further `tools` on PATH, `dependencies` added to the package's, `lock` options
# filled from the package's own lock file, and the `stack` of tools built with the system itself.
# `sh` is for tools that start a shell by name, ninja, npm run and libtool among them.
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
  inherit (lib) on;

  # bin scripts start with #!/usr/bin/env node, and prebuilt .node addons such as rollup's and
  # esbuild's link libgcc_s.so.1, at run time and while building (builder/node-common.nu)
  nodeRun = [
    pkgs.nodejs
    pkgs.libgcc-shim
  ];
  nodeBuild = [ buildPkgs.libgcc-shim ];
  # uv.lock leaves build backends out, so a python app gets every one the set has
  backends = with buildPkgs; [
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
  ];

  bindings = with buildPkgs; {
    # make and bash come from baseTools, or the crib for bootstrapTools packages
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
    # the PEP 517 front end. A member of this stack sees only the members before it
    python = {
      tool = cpython;
      stack = [
        python-flit-core
        python-packaging
        python-pyproject-hooks
        python-build
        python-installer
      ];
      # extension modules link the target's libpython, never the build machine's
      dependencies = on platform.cross [ pkgs.cpython ];
    };
    # what exists before llvm and rust (formatelf, git) sets `cargo.tool = buildPkgs.rust-bootstrap`.
    # A cross build takes std for the target from its own package, <tool>-std
    cargo = {
      tool = rust;
      tools = spec: on platform.cross [ pkgs."${spec.cargo.tool.pname}-std" ];
      lock.deps = fetch.cargoVendor;
    };
    # cabal itself builds with `cabal.tool = buildPkgs.cabal-bootstrap`
    cabal = {
      tool = cabal;
      tools = [
        ghc-bootstrap
        jsem
      ];
      # ghc-bignum links -lgmp into every program and the RTS links -lffi
      dependencies = [
        pkgs.gmp
        pkgs.libffi
      ];
      lock.deps = _: fetch.hackageSet { };
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
      ]
      ++ nodeBuild;
      lock.deps = fetch.pnpmDeps;
      dependencies = nodeRun;
    };
    pyapp = {
      tools = backends ++ on platform.cross [ pkgs.rust-std ];
      lock.deps = args: fetch.pythonDeps (args // { python = pkgs.cpython; });
    };
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
      tools = [ sh ] ++ nodeBuild;
      lock.deps = fetch.bunDeps;
      dependencies = nodeRun;
    };
    yarn = {
      tool = yarn;
      tools = [
        nodejs
        sh
      ]
      ++ nodeBuild;
      lock.deps = fetch.yarnDeps;
      dependencies = nodeRun;
    };
    # escripts start with #!/usr/bin/env escript and releases exec erl
    mix = {
      tool = elixir;
      tools = [
        erlang
        hex
        rebar3
      ];
      lock.deps = fetch.hexDeps;
      dependencies = [ pkgs.erlang ];
    };
    rebar3 = {
      tools = [
        rebar3
        erlang
      ];
      lock.deps = fetch.hexDeps;
      dependencies = [ pkgs.erlang ];
    };
    npm = {
      tool = nodejs;
      tools = [ sh ] ++ nodeBuild;
      lock.deps = fetch.npmDeps;
      dependencies = nodeRun;
    };
  };

  refuses =
    condition:
    if condition == "cross" then
      platform.cross
    else if condition ? libc then
      platform.libc == condition.libc
    else
      platform.libc != condition.libcOtherThan;

  bind =
    name: descriptor:
    let
      given = bindings.${name} or { };
      hasTool = given ? tool;
      options =
        on hasTool {
          tool = {
            type = [ "set" ];
            default = given.tool;
            doc = "the package that provides ${name}";
          };
        }
        // schema.options.${name};
      defaults = builtins.mapAttrs (_: option: option.default) options;
      more = given.tools or [ ];
      moreFor = if builtins.isFunction more then more else _: more;
    in
    {
      inherit options;
      validate =
        args:
        schema.validate (
          {
            system = name;
            extra = on hasTool { tool.kind = "package"; };
          }
          // args
        );
      phases = map (phase: "${name}.${phase}") descriptor.phases;
      # the start of a package's `<name>` record, with the `lock` options fetched from its source
      defaults =
        if given ? lock then
          source: defaults // builtins.mapAttrs (_: fetcher: fetcher { inherit source; }) given.lock
        else
          _: defaults;
      tools = spec: on hasTool [ spec.${name}.tool ] ++ moreFor spec;
      dependencies = given.dependencies or [ ];
      prebuilt = descriptor.prebuilt or false;
      unsupported =
        if descriptor ? unsupported && refuses descriptor.unsupported.when then
          descriptor.unsupported.reason
        else
          null;
      stack = given.stack or [ ];
    };
in
builtins.mapAttrs bind schema.descriptors
