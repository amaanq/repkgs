# From the seed to a configured `cc` per platform:
#   stage0  seed clang, musl    -> jig and a cc for the build machine
#   stage1  seed clang, glibc   -> a cc to host cmake and llvm (clang+lld, all targets)
#   stage2  our clang           -> compiler-rt, libc, libc++, cc and launch for every platform
# Each derivation runs `jig bootstrap <recipe>` with structured attrs. Recipes live in
# pkgs/ji/jig/src/jig/src/bootstrap/.
{
  seed ? null,
  system ? builtins.currentSystem,
}:
let
  platforms = import ../nix/platforms.nix;
  pkg = name: ../pkgs + "/${builtins.substring 0 2 name}/${name}";
  buildPlatform = platforms.forSystem system "glibc";
  # the vendor field says build, so configure sees a cross build even on the same cpu
  buildTriple = builtins.replaceStrings [ "-unknown-" ] [ "-build-" ] buildPlatform.gnuTriple;

  seedBuild = import ../pkgs/se/seed {
    inherit system;
    buildSystem = system;
  };
  # the seed's own sources.toml is read without an unpacker: it is the unpacker
  sourcesOf =
    name:
    (
      if name == "seed" then
        import ../nix/sources.nix { unpacker = null; }
      else
        import ../nix/sources.nix {
          unpacker = seedPath;
          inherit (seedBuild) jig;
          inherit system;
        }
    )
      (pkg name + "/sources.toml");
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

  linuxChain =
    platform: run: ccArgs:
    let
      linux-headers = run [ ] "linux-headers" { };
      built = chain {
        inherit platform run ccArgs;
        libcRecipe = "glibc";
        libcArgs = {
          linuxHeaders = linux-headers;
          # C.UTF-8 is compiled by running the fresh localedef
          locale = platform.clangTarget == buildPlatform.clangTarget;
        };
        linuxHeaders = linux-headers;
      };
    in
    built
    // {
      glibc = built.libc;
      inherit linux-headers;
      launch = run [ built.cc ] "launch" { };
      dlaudit = run [ built.cc ] "dlaudit" { };
    };

  # no jig yet. Linux headers install with musl's own tools here, so they come after libc
  stage0 =
    let
      run = stage {
        platform = platforms.forSystem system "musl";
        suffix = "";
      };
      built = chain {
        inherit run;
        platform = platforms.forSystem system "musl";
        libcRecipe = "musl";
        extraParts = [ linux-headers ];
      };
      linux-headers = run [ ] "linux-headers" {
        sysroot = run [ ] "sysroot" {
          parts = [ built.libc ];
          resource = built.compiler-rt;
        };
      };
      jig = run [ ] "jig" { };
    in
    built
    // {
      inherit jig linux-headers;
      musl = built.libc;
      cc = run [ jig ] "cc" {
        inherit (built) sysroot;
        prebuilt = jig;
      };
    };

  stage1 =
    let
      run = stage {
        platform = buildPlatform;
        tools = [
          stage0.jig
          stage0.cc
        ];
        suffix = "-boot";
      };
      built = linuxChain buildPlatform run { prebuilt = stage0.jig; };
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

  # stage2. stage1's cc stays on PATH as the host compiler (HOSTCC, cc-build)
  stage2For =
    platform:
    {
      sdk ? null, # windows-sdk needs the native set's 7zip, nix/set.nix passes it in
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
      // (if platform == buildPlatform then { } else { native = nativeCc; });
      withLaunch = built: built // { launch = run [ built.cc ] "launch" { }; };
      byLibc = {
        glibc = linuxChain platform run ccArgs;
        msvc = withLaunch (chain {
          inherit platform run ccArgs;
          libcGiven = sdk;
          extraParts = [ (run [ ] "vcruntime" { inherit sdk; }) ];
        });
        apple = chain {
          inherit platform run ccArgs;
          libcGiven = run [ ] "apple-sdk" { };
        };
        mingw = withLaunch (chain {
          inherit platform run ccArgs;
          libcRecipe = "mingw-w64";
        });
      };
    in
    byLibc.${platform.libc};
  toolchain = builtins.mapAttrs (_: stage2For) platforms.byName;
  nativeCc = (toolchain.${buildPlatform.name} { }).cc;
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
  # the linux toolchains by cpu (README)
  stage2 = builtins.listToAttrs (
    map (platform: {
      name = platform.cpu;
      value = toolchain.${platform.name} { };
    }) (builtins.filter (platform: platform.os == "linux") (builtins.attrValues platforms.byName))
  );
}
