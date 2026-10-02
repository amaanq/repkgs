# The seed grown into a configured `cc` for every platform, in three stages:
#   stage0  the seed's clang on musl builds jig and a cc for this machine
#   stage1  the seed's clang on glibc builds a cc that can host cmake and llvm (clang, lld, every target)
#   stage2  that llvm builds compiler-rt, the libc, libc++, cc and launch per platform
# Every derivation runs `jig bootstrap <recipe>` with structured attrs, recipes in
# pkgs/ji/jig/src/jig/src/bootstrap/.
{
  seed ? null,
  system ? builtins.currentSystem,
}:
let
  platforms = import ../nix/platforms.nix;
  pkg = name: ../pkgs + "/${builtins.substring 0 2 name}/${name}";
  host = platforms.forSystem system "glibc";
  # with "build" as the vendor, configure treats even a same-cpu build as a cross one
  buildTriple = builtins.replaceStrings [ "-unknown-" ] [ "-build-" ] host.gnuTriple;

  seedBuild = import ../pkgs/se/seed {
    inherit system;
    buildSystem = system;
  };
  # the seed is the unpacker, so its own recipe.toml has to be read without one
  readerFor =
    name:
    if name == "seed" then
      import ../nix/sources.nix { unpacker = null; }
    else
      import ../nix/sources.nix {
        unpacker = seedPath;
        inherit (seedBuild) jig;
        inherit system;
      };
  sourcesOf = name: readerFor name (pkg name + "/recipe.toml");
  source = name: (sourcesOf name).default;
  seedPath =
    if seed == null then
      (sourcesOf "seed").fetch system
    else if builtins.isString seed then
      builtins.storePath seed
    else
      seed;

  stage = import ./stage.nix {
    inherit system buildTriple;
    seed = seedPath;
    inherit (seedBuild) jig;
    inputs = import ./inputs.nix {
      inherit pkg source;
      prebuiltJig = seedBuild.jig;
    };
  };
  chain = import ./chain.nix { inherit pkg; };

  glibcChain =
    platform: run: ccArgs:
    let
      headers = run [ ] "linux-headers" { };
      built = chain {
        inherit platform run ccArgs;
        libcRecipe = "glibc";
        libcArgs = {
          linuxHeaders = headers;
          # building C.UTF-8 runs the new localedef, which only works natively
          locale = platform.clangTarget == host.clangTarget;
        };
        linuxHeaders = headers;
      };
    in
    built
    // {
      glibc = built.libc;
      linux-headers = headers;
      launch = run [ built.cc ] "launch" { };
      dlaudit = run [ built.cc ] "dlaudit" { };
    };

  # before jig exists. The kernel headers install with musl's own tools, so they follow the libc
  stage0 =
    let
      platform = platforms.forSystem system "musl";
      run = stage {
        inherit platform;
        suffix = "";
      };
      built = chain {
        inherit platform run;
        libcRecipe = "musl";
        extraParts = [ headers ];
      };
      headers = run [ ] "linux-headers" {
        sysroot = run [ ] "sysroot" {
          parts = [ built.libc ];
          resource = built.compiler-rt;
        };
      };
      jig = run [ ] "jig" { };
    in
    built
    // {
      inherit jig;
      linux-headers = headers;
      musl = built.libc;
      cc = run [ jig ] "cc" {
        inherit (built) sysroot;
        prebuilt = jig;
      };
    };

  stage1 =
    let
      run = stage {
        platform = host;
        tools = [
          stage0.jig
          stage0.cc
        ];
        suffix = "-boot";
      };
      built = glibcChain host run { prebuilt = stage0.jig; };
      cmake = run [ built.cc ] "cmake" { inherit (built) sysroot; };
    in
    built
    // {
      inherit cmake;
      llvm = run [
        cmake
        built.cc
      ] "llvm" { inherit (built) sysroot; };
    };

  # stage1's cc stays on PATH as the compiler for this machine (HOSTCC, cc-build)
  stage2For =
    platform:
    {
      # the Windows SDK needs the native set's 7zip, so nix/set.nix fetches and passes it
      sdk ? null,
    }:
    let
      run = stage {
        inherit platform;
        clang = stage1.llvm;
        tools = [
          stage0.jig
          stage1.llvm
          stage1.cc
        ];
      };
      ccArgs = {
        prebuilt = stage0.jig;
      }
      // (if platform == host then { } else { native = nativeCc; });
      launched = built: built // { launch = run [ built.cc ] "launch" { }; };
    in
    if platform.libc == "glibc" then
      glibcChain platform run ccArgs
    else if platform.libc == "msvc" then
      launched (chain {
        inherit platform run ccArgs;
        libcGiven = sdk;
        extraParts = [ (run [ ] "vcruntime" { inherit sdk; }) ];
      })
    else if platform.libc == "apple" then
      chain {
        inherit platform run ccArgs;
        libcGiven = run [ ] "apple-sdk" { };
      }
    else if platform.libc == "mingw" then
      launched (chain {
        inherit platform run ccArgs;
        libcRecipe = "mingw-w64";
      })
    else
      throw "bootstrap: no stage2 for libc ${platform.libc}";
  toolchain = builtins.mapAttrs (_: stage2For) platforms.byName;
  nativeCc = (toolchain.${host.name} { }).cc;
  linuxPlatforms = builtins.filter (platform: platform.os == "linux") (
    builtins.attrValues platforms.byName
  );
in
{
  seed = seedPath;
  inherit buildTriple;
  inherit (seedBuild) jib jig nu;
  inherit
    stage0
    stage1
    toolchain
    source
    ;
  inherit (stage1) llvm;
  # the Linux toolchains by cpu (README)
  stage2 = builtins.listToAttrs (
    map (platform: {
      name = platform.cpu;
      value = toolchain.${platform.name} { };
    }) linuxPlatforms
  );
}
