{
  nixpkgs ? import ../../../nix/nixpkgs.nix,
  system ? builtins.currentSystem,
  buildSystem ? builtins.currentSystem,
}:
let
  cross = system != buildSystem;
  cpu = builtins.head (builtins.split "-" system);

  # a static musl host is a cross target to nixpkgs even when the cpu matches
  musl = import nixpkgs {
    localSystem = buildSystem;
    crossSystem = {
      config = "${cpu}-unknown-linux-musl";
      isStatic = true;
    };
  };
  native = musl.buildPackages;
  inherit (native) lib;
  triple = musl.stdenv.hostPlatform.config;

  source =
    name:
    let
      recipe = fromTOML (builtins.readFile ../../${builtins.substring 0 2 name}/${name}/recipe.toml);
      inherit (recipe.pin) version;
      tag = recipe.pin.tag or version;
      entries = lib.listToAttrs (map (entry: lib.nameValuePair entry.key entry) recipe.source);
      fetch =
        key:
        (native.fetchzip.override { withUnzip = false; }) {
          name = "${name}-${version}-${if key == "default" then "source" else key}";
          url = builtins.replaceStrings [ "{version}" "{tag}" ] [ version tag ] entries.${key}.url;
          inherit (entries.${key}) hash;
        };
    in
    {
      inherit version fetch;
      default = fetch "default";
    };

  llvm =
    let
      src = source "llvm";
      backend =
        {
          x86_64 = "X86";
          aarch64 = "AArch64";
          riscv64 = "RISCV";
          loongarch64 = "LoongArch";
          powerpc64le = "PowerPC";
        }
        .${cpu};
      defs = {
        CMAKE_BUILD_TYPE = "Release";
        CMAKE_INSTALL_PREFIX = "$out";
        CMAKE_C_COMPILER = "$CC";
        CMAKE_CXX_COMPILER = "$CXX";
        CMAKE_AR = "$(command -v $AR)";
        CMAKE_RANLIB = "$(command -v $RANLIB)";
        LLVM_ENABLE_PROJECTS = "clang;lld";
        LLVM_TARGETS_TO_BUILD = backend;
        LLVM_HOST_TRIPLE = triple;
        LLVM_DEFAULT_TARGET_TRIPLE = triple;
        LLVM_TOOL_LLVM_DRIVER_BUILD = "ON";
        LLVM_BUILD_STATIC = "ON";
        LLVM_ENABLE_PIC = "OFF";
        LLVM_PARALLEL_LINK_JOBS = "4";
        LLVM_ENABLE_ZLIB = "FORCE_ON";
        LLVM_ENABLE_ZSTD = "FORCE_ON";
        LLVM_USE_STATIC_ZSTD = "ON";
        LLVM_ENABLE_LIBXML2 = "OFF";
        LLVM_ENABLE_TERMINFO = "OFF";
        LLVM_ENABLE_LIBEDIT = "OFF";
        LLVM_ENABLE_LIBPFM = "OFF";
        LLVM_ENABLE_PLUGINS = "OFF";
        CLANG_PLUGIN_SUPPORT = "OFF";
        LLVM_INCLUDE_TESTS = "OFF";
        LLVM_INCLUDE_BENCHMARKS = "OFF";
        LLVM_INCLUDE_EXAMPLES = "OFF";
        LLVM_INCLUDE_DOCS = "OFF";
        CLANG_INCLUDE_TESTS = "OFF";
        CLANG_INCLUDE_DOCS = "OFF";
        CLANG_ENABLE_ARCMT = "OFF";
        CLANG_ENABLE_STATIC_ANALYZER = "OFF";
        CLANG_TOOL_CLANG_REPL_BUILD = "OFF";
        CLANG_ENABLE_HLSL = "OFF";
        CLANG_DEFAULT_LINKER = "lld";
        CLANG_DEFAULT_RTLIB = "compiler-rt";
        CLANG_DEFAULT_UNWINDLIB = "libunwind";
        CLANG_DEFAULT_CXX_STDLIB = "libc++";
        CLANG_DEFAULT_OBJCOPY = "llvm-objcopy";
        LLVM_INSTALL_TOOLCHAIN_ONLY = "ON";
        LLVM_TOOLCHAIN_TOOLS = lib.concatStringsSep ";" [
          "llvm-ar"
          "llvm-ranlib"
          "llvm-nm"
          "llvm-objcopy"
          "llvm-strip"
          "llvm-objdump"
          "llvm-readelf"
          "llvm-readobj"
          "llvm-size"
          "llvm-strings"
          "llvm-cxxfilt"
        ];
      }
      // lib.optionalAttrs cross {
        CMAKE_SYSTEM_NAME = "Linux";
        CMAKE_SYSTEM_PROCESSOR = cpu;
        # tblgen and friends run during the build, so they need the build machine's compiler
        CROSS_TOOLCHAIN_FLAGS_NATIVE = "-DCMAKE_C_COMPILER=${native.stdenv.cc}/bin/cc;-DCMAKE_CXX_COMPILER=${native.stdenv.cc}/bin/c++";
      };
    in
    musl.stdenv.mkDerivation {
      pname = "peanut-llvm";
      inherit (src) version;
      src = src.default;
      patches = [ ../../ll/llvm/upstream-x86-vastart-stack-probe.patch ];
      nativeBuildInputs = [
        native.cmakeMinimal
        (native.ninja.override {
          buildDocs = false;
          python3 = native.python3Minimal;
        })
        native.python3Minimal
      ];
      buildInputs = [
        musl.zlib
        musl.zstd
      ];
      dontUseCmakeConfigure = true;
      dontFixup = true;
      configurePhase = ''
        runHook preConfigure
        cmake -S llvm -B build -G Ninja ${
          lib.concatStringsSep " " (lib.mapAttrsToList (name: value: ''"-D${name}=${value}"'') defs)
        }
        runHook postConfigure
      '';
      buildPhase = ''
        runHook preBuild
        ninja -C build -j$NIX_BUILD_CORES llvm-driver clang-resource-headers
        runHook postBuild
      '';
      installPhase = ''
        runHook preInstall
        mkdir -p $out/bin $out/lib
        cp build/bin/llvm $out/bin/llvm
        $STRIP $out/bin/llvm

        for tool in $(sed -n 's/^[[:space:]]*LLVM_DRIVER_TOOL("\([^"]*\)".*/\1/p' build/tools/llvm-driver/LLVMDriverTools.def); do
          case $tool in
            dsymutil) continue ;;
            clang | clang-* | lld) ln -sf llvm $out/bin/$tool ;;
            *) ln -sf llvm $out/bin/llvm-$tool ;;
          esac
        done
        for alias in clang++ clang-cpp ld.lld; do
          ln -sf llvm $out/bin/$alias
        done
        for short in ar ranlib nm objcopy objdump strip readelf size strings c++filt; do
          if [ -e $out/bin/llvm-$short ]; then
            ln -sf llvm $out/bin/$short
          fi
        done

        cp -r build/lib/clang $out/lib/clang
        runHook postInstall
      '';
    };

  rustBootstrap = import ./rust.nix {
    ps = musl;
    inherit source;
    pkgs = import nixpkgs { system = buildSystem; };
    # rust's own build runs this llvm, so under cross it is the build machine's peanut llvm
    llvm =
      if cross then
        (import ./build.nix {
          inherit nixpkgs buildSystem;
          system = buildSystem;
        }).llvm
      else
        llvm;
  };

  rustPlatform = musl.makeRustPlatform {
    rustc = rustBootstrap.rust;
    cargo = rustBootstrap.rust;
  };

  workspace = ../../mu/mummer/src;

  crate =
    name: skip:
    rustPlatform.buildRustPackage {
      pname = "peanut-${name}";
      version = "3.0.0";
      src = builtins.path {
        name = "${name}-rust-src";
        path = workspace;
        filter =
          path: type:
          let
            base = baseNameOf path;
          in
          !skip (lib.removePrefix "${toString workspace}/" path)
          && (
            if type == "directory" then
              base != "target"
            else
              lib.hasSuffix ".rs" base
              || builtins.elem base [
                "Cargo.toml"
                "Cargo.lock"
                "README.md"
              ]
          );
      };
      cargoLock.lockFile = workspace + "/Cargo.lock";
      cargoBuildFlags = [
        "-p"
        name
      ];
      auditable = false;
      doCheck = false;
      env.MUMMER_STORE_DIR = builtins.storeDir;
      allowedReferences = [ ];
    };

  # every member keeps its main.rs so the workspace resolves, and edits to the
  # others leave mummer's source, and so the whole bootstrap, unchanged
  mummer = crate "mummer" (
    relative:
    builtins.match "(bigtop|jape|spieler)/src/.+" relative != null && baseNameOf relative != "main.rs"
  );
  bigtop = crate "bigtop" (_: false);

  nu =
    let
      src = source "nushell";
    in
    (musl.nushell.override {
      inherit rustPlatform;
      withDefaultFeatures = false;
      additionalFeatures = _: [
        "network"
        "sqlite"
        "trash-support"
        "rustls-tls"
      ];
    }).overrideAttrs
      {
        pname = "peanut-nushell";
        inherit (src) version;
        src = src.default;
        auditable = false;
        doCheck = false;
        # the generated parser iterates a HashMap, so its output order changes between builds
        postPatch = ''
          substituteInPlace "$cargoDepsCopy/source-registry-0/pest_consume_macros-1.1.0/src/make_parser.rs" \
            --replace-fail HashMap BTreeMap
        '';
      };

  bsdtar = musl.libarchive;
  # the defconfig leaves out pending commands that configure scripts call
  toybox = musl.toybox.override {
    extraConfig = ''
      CONFIG_AWK=y
      CONFIG_DIFF=y
      CONFIG_EXPR=y
      CONFIG_GZIP=y
      CONFIG_TR=y
    '';
  };
  inherit (musl) dash;

  tools = {
    make = musl.gnumake.override { guileSupport = false; };
    gawk = musl.gawk.override { interactive = false; };
    sed = musl.gnused;
    grep = musl.gnugrep.override { pcre2 = null; };
    m4 = musl.gnum4;
    # glibc's intl/plural.y
    bison = musl.bison;
  };

  python = musl.stdenv.mkDerivation {
    name = "peanut-python";
    src = (source "cpython314").default;
    preConfigure = ''
      {
        echo '*disabled*'
        echo ${
          lib.concatStringsSep " " [
            "_ctypes"
            "_ssl"
            "_hashlib"
            "_tkinter"
            "_curses"
            "_curses_panel"
            "_dbm"
            "_gdbm"
            "_lzma"
            "_bz2"
            "zlib"
            "readline"
            "_sqlite3"
            "_uuid"
            "nis"
            "ossaudiodev"
            "spwd"
            "_crypt"
            "_scproxy"
            "xxlimited"
            "xxlimited_35"
            "_testcapi"
            "_testinternalcapi"
            "_testbuffer"
            "_testimportmultiple"
            "_testmultiphase"
            "_testsinglephase"
            "_testclinic"
            "_testexternalinspection"
            "_ctypes_test"
            "_xxtestfuzz"
          ]
        }
      } > Modules/Setup.local
    '';
    configureFlags = [
      "--disable-shared"
      "--without-ensurepip"
      "--disable-test-modules"
      "--without-doc-strings"
      "--without-pymalloc"
      "--with-static-libpython"
      "--without-readline"
      "--without-system-expat"
      "--with-pkg-config=no"
      "ac_cv_func_dlopen=no"
      "ac_cv_lib_dl_dlopen=no"
      "py_cv_module__ctypes=n/a"
      "ac_cv_file__dev_ptmx=yes"
      "ac_cv_file__dev_ptc=no"
      "ac_cv_buggy_getaddrinfo=no"
      "--with-build-python=${native.python314}/bin/python3"
    ];
    env = {
      LDFLAGS = "-static";
      LINKFORSHARED = " ";
      MODULE_BUILDTYPE = "static";
      CFLAGS = "-O2 -w";
    };
    buildFlags = [ "python" ];
    installTargets = "bininstall libinstall inclinstall";
    postInstall = ''
      (
        cd $out/lib/python3.*
        rm -rf test idlelib tkinter turtledemo ensurepip lib2to3 config-3* site-packages
      )
      rm -rf $out/share $out/lib/pkgconfig $out/bin/idle* $out/bin/pydoc* $out/lib/libpython*.a $out/include
      find $out -name __pycache__ -type d -prune -exec rm -rf {} +
    ''
    + lib.optionalString (!cross) ''
      $out/bin/python3 -B -c 'import sys, os, re, json, subprocess, argparse'
    '';
    dontFixup = true;
  };

  peanut = native.runCommand "peanut-4-${system}" { allowedReferences = [ ]; } ''
    mkdir -p $out/bin $out/lib $out/share
    cp ${bsdtar}/bin/bsdtar ${toybox}/bin/toybox ${dash}/bin/dash $out/bin/
    ln -s dash $out/bin/sh
    ${lib.concatMapStrings (name: ''
      cp ${tools.${name}}/bin/${name} $out/bin/
    '') (lib.attrNames tools)}
    cp ${tools.bison}/bin/yacc $out/bin/ 2>/dev/null || true
    cp -r ${tools.bison}/share/bison $out/share/bison
    cp -L ${python}/bin/python3 $out/bin/
    cp -r ${python}/lib/python3.* $out/lib/

    cp -P -n ${toybox}/bin/* $out/bin/
    # configure scripts look for a plain ld
    ln -s ld.lld $out/bin/ld
    cp -a ${llvm}/bin/. $out/bin/
    cp -a ${llvm}/lib/clang $out/lib/
    chmod -R u+w $out
    cp ${mummer}/bin/mummer $out/bin/

    for program in bsdtar toybox dash python3 ${lib.concatStringsSep " " (lib.attrNames tools)}; do
      ${llvm}/bin/llvm-strip $out/bin/$program
    done
    find $out -type f -exec ${native.nukeReferences}/bin/nuke-refs {} +
  '';

  # the hash recipe.toml pins is the NAR hash of the unpacked tree
  nar =
    native.runCommand "${peanut.name}.nar.xz"
      {
        nativeBuildInputs = [
          native.nix
          native.xz
        ];
      }
      ''
        mkdir $out
        nix-store --dump ${peanut} | xz -T$NIX_BUILD_CORES -9e > $out/${peanut.name}.nar.xz
        nix-hash --type sha256 --to-sri "$(nix-store --dump ${peanut} | sha256sum | cut -d' ' -f1)" > $out/nar-hash
      '';
in
{
  inherit
    llvm
    mummer
    bigtop
    nu
    bsdtar
    toybox
    dash
    python
    peanut
    nar
    ;
  inherit (rustBootstrap)
    rust
    stage0
    mrustc
    rustStages
    ;
}
// tools
