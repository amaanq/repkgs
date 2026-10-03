# The `package` function: spec attrset -> derivation (+ `.tests` when tests.separate).
# Validates the spec against the schema jib emits, then hands jib a plan, the build systems to set
# up and the phases in order, which `jib build` runs in one process, spawning nu only for the
# phases written in it. Vocabulary: README.md "Writing a package".
{
  platform,
  toolchain,
  jib,
  seed,
  nu,
  launch,
  dlaudit,
  buildSystems,
  baseTools,
  relocTools,
  # overrides applied to a spec before validation: name -> spec -> spec (nix/overrides.nix)
  edit,
  # [pin] sys in recipe.toml names packages of the set
  pkgs,
  lib,
}:
let
  inherit (builtins)
    all
    any
    attrNames
    attrValues
    concatMap
    elem
    filter
    hasContext
    head
    isList
    isString
    listToAttrs
    mapAttrs
    tail
    ;
  inherit (lib) join;
  schema = import ../schema.nix;
  completions = import ./completions.nix;
  elf = platform.binfmt == "elf";

  # the builder modules every script imports: one string for the set
  tree = builtins.path {
    path = ../../builder;
    name = "build";
  };
  prelude = "use ${tree}/core.nu *\nuse ${tree}/implant.nu";

  # the part of every derivation that is the same across the set: built once
  shared = {
    inherit seed;
    inherit (platform) system;
    __structuredAttrs = true;
    # content-addressed: rebuilds that change no bytes do not propagate
    __contentAddressed = true;
    outputHashMode = "recursive";
    outputHashAlgo = "sha256";
    builder = "${jib}/bin/jib";
    # platform facts autoconf would otherwise probe (or guess, when cross): nix/config.site
    CONFIG_SITE = "${../config.site}";
    platform = (import ../platforms.nix).forBuilder platform // {
      inherit (toolchain) sysroot;
      probe = if platform.cross then "${toolchain.sysroot}/lib/${platform.interp}" else "";
      # `prebuilt`: upstream ELFs get our dynamic linker implanted (true) or via launch ("ldso")
      interp = "${toolchain.sysroot}/lib/${platform.interp}";
      launch =
        if elf then
          "${launch}/bin/launch"
        else if platform.os == "windows" then
          "${launch}/bin/launch.exe"
        else
          "";
      # jib finish runs the version check under it: a failed dlopen fails the build
      dlaudit = if elf then "${dlaudit}/lib/dlaudit.so" else "";
      relocStub = "${toolchain}/lib/reloc_stub.bin";
    };
  };
in
# sources: the package's recipe.toml (nix/sources.nix) or null. It supplies version and source
# unless package.nix sets them (local trees, demos)
# dirs: where a phase prefix that is no build system finds <prefix>.nu, the package's own first
sources0: dirs: features: fn: written:
let
  edited = if edit == null then written else edit written.name written;
  # an override may repin the package: `pin.merge = { version = "…"; }` plus
  # `hash.merge = { default = "sha256-…"; }` re-read recipe.toml under the new [pin]
  repinned = edit != null && sources0 != null && (edited ? pin || edited ? hash);
  sources = if repinned then sources0.repin (edited.pin or { }) (edited.hash or { }) else sources0;
  fromSources =
    if sources == null then
      { }
    else
      {
        inherit (sources) version;
        # one tarball for all, or one per platform ("x86_64-linux", "aarch64-macos")
        source = if sources.has "default" then "default" else "${platform.cpu}-${platform.os}";
      };
  args =
    fromSources
    // (
      if repinned then
        removeAttrs edited [
          "pin"
          "hash"
        ]
      else
        edited
    )
    // (
      if edited ? completions then
        { install = completions edited.completions // (edited.install or { }); }
      else
        { }
    );
  inherit (args) name;
  uses = args.uses or [ ];
  fail = message: throw "${name}: ${message}";

  # `<bs> = on cond { … }` for a build system used on some platforms only leaves { } on the others
  inactive = filter (key: !(elem key schema.recipeFields) && args.${key} == { }) (attrNames args);
  badFields = schema.validateRecipe {
    value = removeAttrs args (uses ++ inactive);
    mode = "keys";
  };
  badOptions = concatMap (
    system:
    buildSystems.${system}.validate {
      value = args.${system} or { };
      mode = "keys";
    }
  ) uses;
  # the shape is the schema's, values from outside are checked where they are resolved (nix/features.nix)
  badFeatures = filter (
    feature:
    let
      declared = args.features.${feature};
      defaults = if isList declared.default then declared.default else [ declared.default ];
    in
    declared ? values && !all (value: elem value declared.values) defaults
  ) (attrNames (args.features or { }));
  # a library among the build tools or a tool among the libraries: natively both platforms
  # coincide and nothing would notice, so it is checked here
  wrongPlatform =
    map (dependency: "dependencies: ${dependency.pname} is built for ${dependency.platform}") (
      filter (dependency: (dependency.platform or platform.name) != platform.name) (
        args.dependencies or [ ]
      )
    )
    ++ map (dependency: "buildDependencies: ${dependency.pname} is built for ${dependency.platform}") (
      filter (dependency: (dependency.platform or platform.system) != platform.system) (
        args.buildDependencies or [ ]
      )
    );
  checks =
    if badFields != [ ] then
      fail (join "; " badFields)
    else if badFeatures != [ ] then
      fail "features ${toString badFeatures}: the default is not among the values"
    else if badOptions != [ ] then
      fail (join "; " badOptions)
    else if wrongPlatform != [ ] then
      fail (join "; " wrongPlatform)
    else
      true;
  # fully checked only where read, since an interpolated string forces its dependency
  valid = mapAttrs (
    field: value:
    let
      errors = schema.checkField "deep" field value;
    in
    if !(elem field schema.recipeFields) || errors == [ ] then value else fail (join "; " errors)
  ) args;

  # `source` as a plain string names a recipe.toml key; a cpu the file has no tarball for is
  # unsupported. Paths and derivations (strings with context) are the source itself
  sourceKey =
    let
      source = args.source or null;
    in
    if isString source && !hasContext source then source else null;
  src = if sourceKey != null then sources.fetch sourceKey else valid.source;
  unsupportedReason = import ./support.nix {
    inherit
      platform
      buildSystems
      name
      args
      uses
      sources
      sourceKey
      ;
    dependencies =
      common.dependencies
      ++ (args.buildDependencies or [ ])
      ++ concatMap (system: buildSystems.${system}.tools spec) uses;
  };
  supported = unsupportedReason == null;

  # tests.separate: the build derivation skips *.test and keeps its tree in output `tree`;
  # `<pkg>.tests` restores it and runs only the test phases: a test failure fails that derivation,
  # not the package, and a retry does not rebuild
  separate = valid.tests.separate or false;
  testsRun = valid.tests.run or true;
  # tools only the suite runs. With tests.separate they stay off the build derivation, which is
  # how the base tools test with perl (built with them) without a cycle
  testDependencies = if testsRun then valid.tests.dependencies or [ ] else [ ];
  phases = import ./phases.nix {
    inherit
      buildSystems
      uses
      args
      fail
      dirs
      prelude
      separate
      ;
    written = valid;
  };

  # a build system's `stack` (tools that are themselves built with it): a member sees only the
  # members before it, everyone else sees all of it
  before =
    stack:
    if stack == [ ] || (head stack).pname == name then [ ] else [ (head stack) ] ++ before (tail stack);
  # an upstream-binary package says `prebuilt`, or one of its build systems does (pyapp: wheels)
  prebuilt = args.prebuilt or (any (system: buildSystems.${system}.prebuilt == true) uses);
  # DWARF goes to the `debug` output (jib finish). false when the build cannot be made to keep it
  debug = args.debug or (prebuilt == false);
  # [pin] sys of recipe.toml. null: no recipe.toml, dependencies are all by hand
  sys = args.sys or (if sources == null then null else sources.sys);
  options =
    system:
    mapAttrs (
      option: value:
      let
        errors = buildSystems.${system}.validate { value.${option} = value; };
      in
      if errors == [ ] then value else fail (join "; " errors)
    ) (buildSystems.${system}.defaults src // (args.${system} or { }));
  spec =
    removeAttrs valid [
      "source"
      "patches"
      "buildDependencies"
      "dependencies"
      "bootstrapTools"
    ]
    // (if args ? tests then { tests = removeAttrs valid.tests [ "dependencies" ]; } else { })
    // listToAttrs (
      map (system: {
        name = system;
        value = options system;
      }) uses
    )
    // {
      # sys for jib syslibs
      inherit prebuilt debug sys;
      phases = phases.list;
    }
    # resolved values, for phases: `(ctx).spec.features.tls`
    // (if features == { } then { } else { inherit features; });
  runsNu =
    any (step: step.kind == "nu") phases.steps
    || any (override: (override.run or "") != "") (attrValues (args.pyapp.overrides or { }));
  # PATH order: the base tools (GNU userland, then the seed) go last and only once, so a build
  # system that lists the seed as its `sh` does not pull toybox ahead of coreutils and grep
  buildDependencies =
    { tests, base }:
    filter (dependency: !(elem dependency base)) (
      [
        toolchain
        jib
      ]
      ++ (valid.buildDependencies or [ ])
      ++ tests
      ++ (if runsNu then [ nu ] else [ ])
      ++ (if prebuilt == true then relocTools else [ ])
      ++ concatMap (system: buildSystems.${system}.tools spec ++ before buildSystems.${system}.stack) uses
    )
    ++ base;
  common = shared // {
    inherit src spec;
    inherit (phases) plan;
    probeInputs = [ tree ] ++ phases.modules;
    inherit (args) version;
    patches = valid.patches or [ ];
    buildDependencies = buildDependencies {
      tests = if separate then [ ] else testDependencies;
      base = if args.bootstrapTools or false then baseTools.bootstrap else baseTools.full;
    };
    # [pin] sys: libraries the lock files can link (builder/sys-libs.json). Those the set lacks
    # here are left to the locked package (vendored copy or feature off)
    dependencies =
      (valid.dependencies or [ ])
      ++ map (library: pkgs.${library}) (
        filter (library: pkgs.${library}.supported or false) (if sys == null then [ ] else sys)
      )
      ++ concatMap (system: buildSystems.${system}.dependencies) uses;
  };
  drv = derivation (
    common
    // {
      name = if platform.cross then "${name}-${platform.name}" else name;
      outputs = [
        "out"
        "debug"
      ]
      ++ (if separate then [ "tree" ] else [ ]);
      args = [ "build" ];
    }
  );
  testsDrv = derivation (
    common
    // {
      name = "${drv.name}-tests";
      outputs = [ "out" ];
      buildDependencies = buildDependencies {
        tests = testDependencies;
        base = baseTools.full;
      };
      package = drv.out;
      args = [
        "build"
        "--tests"
        "--from-tree"
        "${drv.tree}"
      ];
    }
  );
  refused = listToAttrs (
    map
      (attribute: {
        name = attribute;
        value = throw unsupportedReason;
      })
      (
        [
          "drvPath"
          "outPath"
          "tests"
        ]
        ++ drv.outputs
      )
  );
in
assert checks;
drv
// {
  pname = name;
  platform = platform.name;
  # what package.nix wrote and read, for `variant`
  args = written;
  sources = sources0;
  decl = written.features or { };
  inherit (phases) script;
  inherit
    dirs
    features
    fn
    supported
    unsupportedReason
    ;
}
// (if separate then { tests = testsDrv; } else { })
// (if supported then { } else refused)
