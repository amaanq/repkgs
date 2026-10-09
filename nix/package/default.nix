# The `package` function: spec attrset -> derivation (+ `.tests` when tests.separate).
# Validates the spec against the schema bigtop emits, then hands bigtop a plan, the build systems to set
# up and the phases in order, which `bigtop build` runs in one process, spawning nu only for the
# phases written in it. Vocabulary: README.md "Writing a package".
{
  platform,
  toolchain,
  bigtop,
  peanut,
  nu,
  launch,
  dlaudit,
  buildSystems,
  baseTools,
  relocTools,
  # overrides applied to a spec before validation: name -> spec -> spec (nix/overrides.nix)
  edit,
  # act.lock's sys names packages of the set
  pkgs,
  lib,
}:
let
  inherit (builtins)
    any
    attrValues
    concatMap
    elem
    filter
    hasContext
    head
    isString
    listToAttrs
    mapAttrs
    tail
    ;
  inherit (lib) join;
  resolve = import ./resolve.nix { inherit platform lib edit; };
  validate = import ./validate.nix {
    inherit platform buildSystems lib;
    schema = import ../schema.nix;
  };
  elf = platform.binfmt == "elf";

  # the builder modules every script imports: one string for the set
  tree = builtins.path {
    path = ../../builder;
    name = "build";
  };
  prelude = "use ${tree}/core.nu *\nuse ${tree}/implant.nu";

  # the part of every derivation that is the same across the set: built once
  shared = {
    inherit peanut;
    inherit (platform) system;
    __structuredAttrs = true;
    # content-addressed: rebuilds that change no bytes do not propagate
    __contentAddressed = true;
    outputHashMode = "recursive";
    outputHashAlgo = "sha256";
    builder = "${bigtop}/bin/bigtop";
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
      # bigtop finish runs the version check under it: a failed dlopen fails the build
      dlaudit = if elf then "${dlaudit}/lib/dlaudit.so" else "";
      relocStub = "${toolchain}/lib/reloc_stub.bin";
    };
  };
in
# sources: the package's act.toml (nix/sources.nix) or null
# dirs: where a phase prefix that is no build system finds <prefix>.nu, the package's own first
{
  sources,
  dirs,
  features,
  fn,
}:
written:
let
  resolved = resolve { inherit sources written; };
  inherit (resolved) args;
  pinned = resolved.sources;
  inherit (args) name;
  uses = args.uses or [ ];
  fail = message: throw "${name}: ${message}";
  checked = validate { inherit args uses fail; };
  checks = lib.check name checked.errors;
  inherit (checked) valid;

  # `source` as a plain string names an act.toml source; a cpu the file has no tarball for is
  # unsupported. Paths and derivations (strings with context) are the source itself
  sourceKey =
    let
      source = args.source or null;
    in
    if isString source && !hasContext source then source else null;
  src = if sourceKey != null then pinned.fetch sourceKey else valid.source;
  unsupportedReason = import ./support.nix {
    inherit
      platform
      buildSystems
      name
      args
      uses
      sourceKey
      ;
    sources = pinned;
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
  # DWARF goes to the `debug` output (bigtop finish). false when the build cannot be made to keep it
  debug = args.debug or (prebuilt == false);
  # act.lock's sys. null: no act.toml, dependencies are all by hand
  sys = args.sys or (if pinned == null then null else pinned.sys);
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
      # sys for bigtop syslibs
      inherit prebuilt debug sys;
      phases = phases.list;
    }
    # resolved values, for phases: `(ctx).spec.features.tls`
    // (if features == { } then { } else { inherit features; });
  runsNu =
    any (step: step.kind == "nu") phases.steps
    || any (override: (override.run or "") != "") (attrValues (args.pyapp.overrides or { }));
  # PATH order: the base tools (GNU userland, then the peanut) go last and only once, so a build
  # system that lists the peanut as its `sh` does not pull toybox ahead of coreutils and grep
  buildDependencies =
    { tests, base }:
    filter (dependency: !(elem dependency base)) (
      [
        toolchain
        bigtop
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
    # act.lock's sys: libraries the lock files can link (builder/sys-libs.toml). Those the set lacks
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
  inherit sources;
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
