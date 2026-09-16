# The `package` function: spec attrset -> derivation (+ `.tests` when tests.separate).
# Validates field and option names at eval time, then hands jib a plan, the build systems to set
# up and the phases in order, which `jib build` runs in one process, spawning nu only for the
# phases written in it. Vocabulary: README.md "Writing a package".
{
  platform,
  toolchain,
  jib,
  nu,
  launch,
  dlaudit,
  buildSystems,
  baseTools,
  relocTools,
  # overrides applied to a spec before validation: name -> spec -> spec (nix/overrides.nix)
  edit,
  # [pin] sys in sources.toml names packages of the set
  pkgs,
  lib,
}:
let
  elf = platform.binfmt == "elf";
  hardening = import ./hardening.nix;
  inherit (lib)
    join
    lines
    ;
  inherit (builtins)
    all
    any
    attrNames
    concatMap
    elem
    elemAt
    filter
    foldl'
    head
    hasContext
    isAttrs
    isList
    isString
    length
    listToAttrs
    match
    ;

  # the builder modules every script imports: one string for the set
  preludeCommon = "use ${tree}/core.nu *\nuse ${tree}/implant.nu";
  tree = builtins.path {
    path = ../builder;
    name = "build";
  };
  reserved = [
    "name"
    "features"
    "platforms"
    "version"
    "source"
    "patches"
    "uses"
    "phases"
    "buildDependencies"
    "dependencies"
    "sys"
    "bin"
    "tests"
    "exports"
    "env"
    "root"
    "cc"
    "bootstrapTools"
    "prebuilt"
    "debug"
    "install"
    "links"
    "completions"
  ];

  # the part of every derivation that is the same across the set: built once
  setCommon = {
    inherit (platform) system;
    __structuredAttrs = true;
    # content-addressed: rebuilds that change no bytes do not propagate
    __contentAddressed = true;
    outputHashMode = "recursive";
    outputHashAlgo = "sha256";
    builder = "${jib}/bin/jib";
    # platform facts autoconf would otherwise probe (or guess, when cross): nix/config.site
    CONFIG_SITE = "${../nix/config.site}";
    platform = {
      inherit (platform)
        name
        cpu
        os
        binfmt
        names
        osNames
        clangTarget
        gnuTriple
        rustTriple
        opensslTarget
        buildRustTriple
        buildTriple
        bits
        endian
        cross
        emulator
        abi
        libc
        posix
        ext
        hardening
        ;
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
    # nix/hardening.nix as data for jib env: the flag table and what is on for this platform
    hardening = {
      inherit (hardening) flags cxx link;
      enabled = hardening.forPlatform platform;
    };
  };
  phaseRe = "([a-z][a-z0-9]*)\\.([a-zA-Z][a-zA-Z-]*)";
in
# sources: the package's sources.toml (nix/sources.nix) or null. It supplies version and source
# unless package.nix sets them (local trees, demos)
# dir: the package's directory, where a phase prefix that is no build system finds <prefix>.nu
sources0: dir: features: fn: args0:
let
  edited = if edit == null then args0 else edit args0.name args0;
  # an override may repin the package: `pin.merge = { version = "…"; }` plus
  # `hash.merge = { default = "sha256-…"; }` re-read sources.toml under the new [pin]
  repinned = edit != null && sources0 != null && (edited ? pin || edited ? hash);
  sources = if repinned then sources0.repin (edited.pin or { }) (edited.hash or { }) else sources0;
  # `completions.<shell> = [ files ]`, sugar for `install`: each file goes where that shell
  # looks. bash and zsh find completions by command name (`<cmd>`, `_<cmd>`), fish and nu keep the
  # file name. A ./path beside package.nix arrives with a store hash prefix, dropped here
  completionDirs = {
    bash = "share/bash-completion/completions";
    zsh = "share/zsh/site-functions";
    fish = "share/fish/vendor_completions.d";
    nu = "share/nushell/vendor/autoload";
  };
  completionName =
    shell: f:
    let
      m = match "([a-z0-9]{32}-)?((.*)\\.${shell}|.*)" (baseNameOf f);
      stem = if elemAt m 2 != null then elemAt m 2 else elemAt m 1;
    in
    if shell == "bash" then
      stem
    else if shell == "zsh" then
      (if match "_.*" stem != null then stem else "_" + stem)
    else
      elemAt m 1;
  completionsInstall =
    shells:
    listToAttrs (
      concatMap (
        shell:
        map (f: {
          name = "${completionDirs.${shell}}/${completionName shell f}";
          value = f;
        }) shells.${shell}
      ) (attrNames shells)
    );
  args =
    (
      if sources == null then
        { }
      else
        {
          inherit (sources) version;
          # one tarball for all, or one per platform ("x86_64-linux", "aarch64-macos")
          source = if sources.has "default" then "default" else "${platform.cpu}-${platform.os}";
        }
    )
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
        { install = completionsInstall edited.completions // (edited.install or { }); }
      else
        { }
    );
  inherit (args) name;
  uses = args.uses or [ ];
  fail = msg: throw "${name}: ${msg}";

  unknownUses = filter (u: !(buildSystems ? ${u})) uses;
  # fields whose sub-keys are a fixed vocabulary: a typo there is as silent as one at top level
  subFields = {
    tests = [
      "run"
      "separate"
      "parallel"
      "version"
      "dlopen"
      "dependencies"
    ];
    cc = [
      "cflags"
      "cxxflags"
      "ldflags"
      "hardening"
    ];
    "cc.hardening" = attrNames hardening.flags;
    completions = attrNames completionDirs;
  };
  subKeys =
    prefix: set:
    if isAttrs set then
      concatMap (
        k:
        let
          path = "${prefix}.${k}";
        in
        if !(elem k subFields.${prefix}) then
          [ path ]
        else if subFields ? ${path} then
          subKeys path set.${k}
        else
          [ ]
      ) (attrNames set)
    else
      [ ];
  # `<bs> = on cond { … }` for a build system used on some platforms only leaves { } on the others
  unknownFields =
    filter (k: args.${k} != { }) (attrNames (removeAttrs args (reserved ++ uses)))
    ++ (if args ? tests then subKeys "tests" args.tests else [ ])
    ++ (if args ? cc then subKeys "cc" args.cc else [ ])
    ++ (if args ? completions then subKeys "completions" args.completions else [ ]);
  badOptions = concatMap (
    system:
    let
      declared = buildSystems.${system}.options;
    in
    concatMap (
      key:
      let
        got = builtins.typeOf args.${system}.${key};
        want = declared.${key}.type;
      in
      if !(declared ? ${key}) then
        [ "unknown option ${system}.${key} (have: ${toString (attrNames declared)})" ]
      else if !(elem got want) then
        [ "option ${system}.${key} is a ${got}, expected ${join " or " want}" ]
      else
        [ ]
    ) (attrNames (args.${system} or { }))
  ) (filter (system: buildSystems ? ${system}) uses);
  # a library among the build tools or a tool among the libraries: natively both platforms
  # coincide and nothing would notice, so it is checked here
  wrongPlatform =
    map (d: "dependencies: ${d.pname} is built for ${d.platform}") (
      filter (d: (d.platform or platform.name) != platform.name) (args.dependencies or [ ])
    )
    ++ map (d: "buildDependencies: ${d.pname} is built for ${d.platform}") (
      filter (d: (d.platform or platform.system) != platform.system) (args.buildDependencies or [ ])
    );
  # `supported` without forcing the derivation (docs/design.md): platforms.{cpu,os,abi,libc,posix,cross},
  # a per-cpu tarball in sources.toml, and the dependencies' own verdicts
  badCpu = args ? platforms.cpu && !(elem platform.cpu args.platforms.cpu);
  badOs = args ? platforms.os && !(elem platform.os args.platforms.os);
  badAbi = args ? platforms.abi && !(elem platform.abi args.platforms.abi);
  badLibc = args ? platforms.libc && !(elem platform.libc args.platforms.libc);
  needsPosix = (args.platforms.posix or false) && !platform.posix;
  nativeOnly = (args.platforms.cross or true) == false && platform.cross;
  bsReasons = filter (r: r != null) (map (u: buildSystems.${u}.unsupported) uses);
  # `source` as a plain string names a sources.toml key; a cpu the file has no tarball for is
  # unsupported. Paths and derivations (strings with context) are the source itself
  sourceKey =
    let
      s = args.source or null;
    in
    if isString s && !hasContext s then s else null;
  noTarball = sourceKey != null && !(sources.has sourceKey);
  src = if sourceKey != null then sources.fetch sourceKey else args.source;
  unsupportedDeps = filter (d: !(d.supported or true)) (
    common.dependencies
    ++ (args.buildDependencies or [ ])
    ++ concatMap (u: buildSystems.${u}.tools spec) uses
  );
  unsupportedReason =
    if badCpu then
      "${name}: not for ${platform.cpu} (platforms.cpu)"
    else if badOs then
      "${name}: not for ${platform.os} (platforms.os)"
    else if badAbi then
      "${name}: not for the ${platform.abi} ABI (platforms.abi)"
    else if badLibc then
      "${name}: not on ${platform.libc} (platforms.libc)"
    else if needsPosix then
      "${name}: needs a POSIX system (platforms.posix)"
    else if nativeOnly then
      "${name}: runs its own binaries while installing, cannot be cross-built (platforms.cross)"
    else if bsReasons != [ ] then
      "${name}: ${head bsReasons}"
    else if noTarball then
      "${name}: sources.toml has no '${sourceKey}' source"
    else if unsupportedDeps != [ ] then
      "${name} -> ${(head unsupportedDeps).unsupportedReason}"
    else
      null;
  supported = unsupportedReason == null;
  unknownPlatformKeys = attrNames (
    removeAttrs (args.platforms or { }) [
      "cpu"
      "os"
      "abi"
      "libc"
      "posix"
      "cross"
    ]
  );

  # the declaration's shape. Values from outside are checked where they are resolved (nix/features.nix)
  badFeatures =
    if args ? features then
      filter (
        n:
        let
          d = args.features.${n};
          t = builtins.typeOf d.default;
        in
        !(
          isAttrs d
          && d ? default
          &&
            removeAttrs d [
              "default"
              "values"
              "doc"
            ] == { }
          && elem t [
            "bool"
            "string"
            "list"
            "int"
          ]
          && (
            !(d ? values)
            || isList d.values && all (v: elem v d.values) (if t == "list" then d.default else [ d.default ])
          )
        )
      ) (attrNames args.features)
    else
      [ ];
  checks =
    if badFeatures != [ ] then
      fail "features ${toString badFeatures}: want { default (bool, string, list or int), values? (a list the default is from), doc? }"
    else if unknownUses != [ ] then
      fail "unknown build systems ${toString unknownUses} (have: ${toString (attrNames buildSystems)})"
    else if unknownFields != [ ] || unknownPlatformKeys != [ ] then
      fail "unknown fields ${toString (unknownFields ++ map (k: "platforms.${k}") unknownPlatformKeys)}"
    else if badOptions != [ ] then
      fail (join "; " badOptions)
    else if wrongPlatform != [ ] then
      fail (join "; " wrongPlatform)
    else
      true;

  # `install`/`links` alone (prebuilt binaries, data): the one phase is copying them into $out.
  # `phases` is the whole list, or edits to the first build system's list (README):
  # { before.<phase> = [..]; after.<phase> = [..]; replace.<phase> = phase | [..]; remove = [..]; }
  phases =
    if (args.phases or [ ]) == [ ] then
      if length uses == 1 then
        buildSystems.${head uses}.phases
      else if uses == [ ] && (args ? install || args ? links) then
        [ ]
      else
        fail "'phases' is required with more than one build system (a list, or edits to the first one's)"
    else if isList args.phases then
      args.phases
    else
      let
        e = args.phases;
        known = buildSystems.${head uses}.phases;
        # an edit gated off with `on cond [ … ]` names a phase of the other platform's build system
        nonEmpty = set: filter (n: set.${n} != [ ]) (attrNames set);
        named =
          nonEmpty (e.before or { })
          ++ nonEmpty (e.after or { })
          ++ attrNames (e.replace or { })
          ++ (e.remove or [ ]);
        unknown = filter (n: !elem n known) named;
        bad = filter (
          n:
          !elem n [
            "before"
            "after"
            "replace"
            "remove"
          ]
        ) (attrNames e);
        asList = x: if isList x then x else [ x ];
      in
      if bad != [ ] then
        fail "phases: unknown edit ${head bad} (before, after, replace, remove)"
      else if unknown != [ ] then
        fail "phases: ${head unknown} is not a phase of ${head uses} (${toString known})"
      else
        concatMap (
          p:
          if elem p (e.remove or [ ]) then
            [ ]
          else
            asList (e.before.${p} or [ ]) ++ asList (e.replace.${p} or p) ++ asList (e.after.${p} or [ ])
        ) known;
  # a build system's `stack` (tools that are themselves built with it): a member sees only the
  # members before it, everyone else sees all of it
  stackBefore =
    stack:
    let
      r =
        foldl' (a: p: if a.hit || p.pname == name then a // { hit = true; } else a // { l = a.l ++ [ p ]; })
          {
            hit = false;
            l = [ ];
          }
          stack;
    in
    r.l;
  # tests.separate: the build derivation skips *.test and keeps its tree in output `tree`;
  # `<pkg>.tests` restores it and runs only the test phases: a test failure fails that derivation,
  # not the package, and a retry does not rebuild
  separate = args.tests.separate or false;
  # tools only the suite runs. With tests.separate they stay off the build derivation, which is
  # how the base tools test with perl (built with them) without a cycle
  testDeps = if testsRun then args.tests.dependencies or [ ] else [ ];

  # a phase as a plan step for jib: a build system's verb runs in jib, anything else is nu text,
  # inline or a package module's command. A prefix that is neither: nix reports "path …/<bs>.nu
  # does not exist". An inline phase's env changes carry over to later phases, its cwd does not
  step =
    s:
    if isAttrs s then
      {
        kind = "nu";
        inherit (s) name;
        test = s.name == "test";
        body = "do --env {\nlet c = (ctx)\n${s.run}\n}";
      }
    else
      let
        p = match phaseRe s;
      in
      if p == null then
        fail "phase '${s}' is not <build system or module>.<phase>"
      else if elem (elemAt p 0) uses then
        {
          kind = "system";
          name = s;
          system = elemAt p 0;
          verb = elemAt p 1;
          test = elemAt p 1 == "test";
        }
      else
        {
          kind = "nu";
          name = s;
          test = elemAt p 1 == "test";
          body = "${elemAt p 0} ${elemAt p 1}";
        };
  # a phase "zig.restore" whose prefix is no `uses` entry is the package's own module zig.nu next
  # to package.nix, for phases too long to read inline. It imports the builder by bare name
  modules =
    if !(args ? phases) then
      [ ]
    else
      foldl' (acc: m: if m == null || elem m (uses ++ acc) then acc else acc ++ [ m ]) [ ] (
        map (
          s:
          let
            p = if isAttrs s then null else match phaseRe s;
          in
          if p == null then null else head p
        ) phases
      );
  # <store dir>/<m>.nu: nu names a module after its file, a bare store path would be <hash>-<m>
  storeModule =
    m:
    "${
      builtins.path {
        path = dir;
        name = "module";
        filter = p: _: baseNameOf p == "${m}.nu";
      }
    }/${m}.nu";
  prelude = lines (
    [
      preludeCommon
    ]
    ++ map (m: "use ${storeModule m}") modules
  );
  plan = {
    inherit prelude separate;
    setups = uses;
    phases = map step phases;
  };
  # what nu parses of a package, for lints/package-scripts.nu
  script = lines ([ prelude ] ++ map (p: p.body) (filter (p: p.kind == "nu") plan.phases));
  # an upstream-binary package says `prebuilt`, or one of its build systems does (pyapp: wheels)
  prebuilt = args.prebuilt or (any (u: buildSystems.${u}.prebuilt == true) uses);
  # DWARF goes to the `debug` output (jib finish). false when the build cannot be made to keep it
  debug = args.debug or (prebuilt == false);
  # [pin] sys of sources.toml. null: no sources.toml, dependencies are all by hand
  sys = args.sys or (if sources == null then null else sources.sys);
  spec =
    removeAttrs args [
      "source"
      "patches"
      "buildDependencies"
      "dependencies"
      "bootstrapTools"
    ]
    // (if args ? tests then { tests = removeAttrs args.tests [ "dependencies" ]; } else { })
    // listToAttrs (
      map (u: {
        name = u;
        value = buildSystems.${u}.defaults src // (args.${u} or { });
      }) uses
    )
    // {
      # sys for jib syslibs
      inherit
        phases
        prebuilt
        debug
        sys
        ;
    }
    # resolved values, for phases: `(ctx).spec.features.tls`
    // (if features == { } then { } else { inherit features; });
  # PATH order: the base tools (GNU userland, then the seed) go last and only once, so a build
  # system that lists the seed as its `sh` does not pull toybox ahead of coreutils and grep
  buildDeps =
    { tests, base }:
    filter (d: !(elem d base)) (
      [ toolchain jib ]
      ++ (args.buildDependencies or [ ])
      ++ tests
      ++ (if any (p: p.kind == "nu") plan.phases then [ nu ] else [ ])
      ++ (if prebuilt == true then relocTools else [ ])
      ++ concatMap (u: buildSystems.${u}.tools spec ++ stackBefore buildSystems.${u}.stack) uses
    )
    ++ base;
  common = setCommon // {
    inherit src;
    probeInputs = [ tree ] ++ map storeModule modules;
    inherit (args) version;
    patches = args.patches or [ ];
    inherit spec plan;
    buildDependencies = buildDeps {
      tests = if separate then [ ] else testDeps;
      base = if args.bootstrapTools or false then baseTools.bootstrap else baseTools.full;
    };
    # [pin] sys: libraries the lock files can link (pkgs/up/uptrack/src/sys-libs.nu). Those the set lacks
    # here are left to the locked package (vendored copy or feature off)
    dependencies =
      (args.dependencies or [ ])
      ++ map (n: pkgs.${n}) (filter (n: pkgs.${n}.supported or false) (if sys == null then [ ] else sys))
      ++ concatMap (u: buildSystems.${u}.dependencies) uses;
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
      buildDependencies = buildDeps {
        tests = testDeps;
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
in
assert checks;
drv
// {
  pname = name;
  platform = platform.name;
  # what package.nix wrote and read, for `variant`
  args = args0;
  sources = sources0;
  decl = args0.features or { };
  inherit dir features fn;
  inherit supported unsupportedReason script;
}
// (if separate then { tests = testsDrv; } else { })
// (
  if supported then
    { }
  else
    listToAttrs (
      map
        (n: {
          name = n;
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
    )
)
