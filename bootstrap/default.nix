# The peanut grown into a compiler per platform, by way of a musl starter and a glibc host with llvm
{
  peanut ? null,
  system ? builtins.currentSystem,
}:
let
  platforms = import ../nix/platforms.nix;
  hostPlatform = platforms.forSystem system "glibc";
  built = import ../pkgs/pe/peanut {
    inherit system;
    buildSystem = system;
  };

  # configure treats a `build` vendor as another machine, so even a same-cpu build cross compiles
  buildTriple = builtins.replaceStrings [ "-unknown-" ] [ "-build-" ] hostPlatform.gnuTriple;
  pkg = name: ../pkgs + "/${builtins.substring 0 2 name}/${name}";

  # the peanut is itself the unpacker, so its own sources are read without one
  bare = import ../nix/sources.nix { unpacker = null; };
  seed =
    if peanut == null then
      (bare (pkg "peanut")).fetch system
    else if builtins.isString peanut then
      builtins.storePath peanut
    else
      peanut;
  reader = import ../nix/sources.nix {
    unpacker = seed;
    inherit (built) mummer;
    inherit system;
  };
  source = name: (reader (pkg name)).default;

  stage = import ./stage.nix {
    inherit system buildTriple;
    peanut = seed;
    inherit (built) mummer;
    inputs = import ./inputs.nix {
      inherit pkg source;
      prebuiltShim = built.mummer;
    };
  };
  chain = import ./chain.nix { inherit pkg; };

  withGlibc =
    platform: run: ccArgs:
    let
      headers = run [ ] "linux-headers" { };
      parts = chain {
        inherit platform run ccArgs;
        libcRecipe = "glibc";
        linuxHeaders = headers;
        # C.UTF-8 is built by running the new localedef, which only runs on this machine
        libcArgs = {
          linuxHeaders = headers;
          locale = platform.clangTarget == hostPlatform.clangTarget;
        };
      };
    in
    parts
    // {
      glibc = parts.libc;
      linux-headers = headers;
      launch = run [ parts.cc ] "launch" { };
      dlaudit = run [ parts.cc ] "dlaudit" { };
    };

  starter =
    let
      platform = platforms.forSystem system "musl";
      run = stage {
        inherit platform;
        role = "starter";
      };
      parts = chain {
        inherit platform run;
        libcRecipe = "musl";
        extraParts = [ headers ];
      };
      headers = run [ ] "linux-headers" {
        sysroot = run [ ] "sysroot" {
          parts = [ parts.libc ];
          resource = parts.compiler-rt;
        };
      };
      mummer = run [ ] "mummer" { };
    in
    parts
    // {
      inherit mummer;
      linux-headers = headers;
      musl = parts.libc;
      cc = run [ mummer ] "cc" {
        inherit (parts) sysroot;
        prebuilt = mummer;
      };
    };

  host =
    let
      run = stage {
        platform = hostPlatform;
        tools = [
          starter.mummer
          starter.cc
        ];
        role = "host";
      };
      parts = withGlibc hostPlatform run { prebuilt = starter.mummer; };
      cmake = run [ parts.cc ] "cmake" { inherit (parts) sysroot; };
    in
    parts
    // {
      inherit cmake;
      llvm = run [
        cmake
        parts.cc
      ] "llvm" { inherit (parts) sysroot; };
    };

  # nix/set.nix passes the Windows SDK in, since unpacking it takes the native set's 7zip
  toolchain = builtins.mapAttrs (
    _: platform:
    {
      sdk ? null,
    }:
    let
      run = stage {
        inherit platform;
        clang = host.llvm;
        tools = [
          starter.mummer
          host.llvm
          host.cc
        ];
      };
      ccArgs = {
        prebuilt = starter.mummer;
      }
      // (
        if platform != hostPlatform then { native = (toolchain.${hostPlatform.name} { }).cc; } else { }
      );
      launching = parts: parts // { launch = run [ parts.cc ] "launch" { }; };
    in
    if platform.libc == "glibc" then
      withGlibc platform run ccArgs
    else if platform.libc == "msvc" then
      launching (chain {
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
      launching (chain {
        inherit platform run ccArgs;
        libcRecipe = "mingw-w64";
      })
    else
      throw "bootstrap: no toolchain for libc ${platform.libc}"
  ) platforms.byName;
in
{
  peanut = seed;
  inherit
    buildTriple
    starter
    host
    toolchain
    source
    ;
  inherit (built) bigtop mummer nu;
  inherit (host) llvm;
}
