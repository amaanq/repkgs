# The seed: the static musl programs for the build machine that the set needs before it can build
# its own userland, built with nixpkgs. It runs on `system` and is built on `buildSystem`, and
# static musl makes that a cross build either way. ./upload.nu builds `nar` per system, uploads it
# and rewrites ./recipe.toml.
{
  nixpkgs ? import ../../../nix/nixpkgs.nix,
  system ? builtins.currentSystem,
  buildSystem ? builtins.currentSystem,
}:
let
  cross = system != buildSystem;
  cpu = builtins.head (builtins.split "-" system);
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

  # a recipe.toml's pinned sources, fetched with nixpkgs, since the seed exists before shim does
  source =
    name:
    let
      manifest = fromTOML (
        builtins.readFile (../.. + "/${builtins.substring 0 2 name}/${name}/recipe.toml")
      );
      inherit (manifest.pin) version;
      tag = manifest.pin.tag or version;
      fetch =
        key:
        let
          entry = builtins.head (builtins.filter (candidate: candidate.key == key) manifest.source);
        in
        (native.fetchzip.override { withUnzip = false; }) {
          name = "${name}-${version}-${if key == "default" then "source" else key}";
          url = builtins.replaceStrings [ "{version}" "{tag}" ] [ version tag ] entry.url;
          inherit (entry) hash;
        };
    in
    {
      inherit version fetch;
      default = fetch "default";
    };

  # stage0 and stage1 build for this machine only (bootstrap/default.nix), so one backend suffices
  backend =
    {
      x86_64 = "X86";
      aarch64 = "AArch64";
      riscv64 = "RISCV";
      loongarch64 = "LoongArch";
      powerpc64le = "PowerPC";
    }
    .${cpu};
  llvmSource = source "llvm";
  # configured as a native build since the static binary runs on the machine it targets. For
  # another cpu, llvm builds its tblgen tools in a NATIVE subtree with this machine's compiler
  llvm = musl.stdenv.mkDerivation {
    pname = "seed-llvm";
    inherit (llvmSource) version;
    src = llvmSource.default;
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
    configurePhase = ''
      cmake -S llvm -B build -G Ninja \
        -DCMAKE_BUILD_TYPE=Release -DCMAKE_INSTALL_PREFIX=$out \
        -DCMAKE_C_COMPILER=$CC -DCMAKE_CXX_COMPILER=$CXX -DCMAKE_AR=$(command -v $AR) -DCMAKE_RANLIB=$(command -v $RANLIB) \
        ${lib.optionalString cross "-DCMAKE_SYSTEM_NAME=Linux -DCMAKE_SYSTEM_PROCESSOR=${cpu} -DCROSS_TOOLCHAIN_FLAGS_NATIVE='-DCMAKE_C_COMPILER=${native.stdenv.cc}/bin/cc;-DCMAKE_CXX_COMPILER=${native.stdenv.cc}/bin/c++'"} \
        -DLLVM_ENABLE_PROJECTS="clang;lld" \
        -DLLVM_TARGETS_TO_BUILD="${backend}" \
        -DLLVM_HOST_TRIPLE=${triple} -DLLVM_DEFAULT_TARGET_TRIPLE=${triple} \
        -DLLVM_TOOL_LLVM_DRIVER_BUILD=ON \
        -DLLVM_BUILD_STATIC=ON -DLLVM_ENABLE_PIC=OFF -DLLVM_PARALLEL_LINK_JOBS=4 \
        -DLLVM_ENABLE_ZLIB=FORCE_ON -DLLVM_ENABLE_ZSTD=FORCE_ON -DLLVM_USE_STATIC_ZSTD=ON \
        -DLLVM_ENABLE_LIBXML2=OFF -DLLVM_ENABLE_TERMINFO=OFF -DLLVM_ENABLE_LIBEDIT=OFF -DLLVM_ENABLE_LIBPFM=OFF \
        -DLLVM_ENABLE_PLUGINS=OFF -DCLANG_PLUGIN_SUPPORT=OFF \
        -DLLVM_INCLUDE_TESTS=OFF -DLLVM_INCLUDE_BENCHMARKS=OFF -DLLVM_INCLUDE_EXAMPLES=OFF -DLLVM_INCLUDE_DOCS=OFF \
        -DCLANG_INCLUDE_TESTS=OFF -DCLANG_INCLUDE_DOCS=OFF -DCLANG_ENABLE_ARCMT=OFF -DCLANG_ENABLE_STATIC_ANALYZER=OFF \
        -DCLANG_TOOL_CLANG_REPL_BUILD=OFF -DCLANG_ENABLE_HLSL=OFF \
        -DCLANG_DEFAULT_LINKER=lld -DCLANG_DEFAULT_RTLIB=compiler-rt -DCLANG_DEFAULT_UNWINDLIB=libunwind \
        -DCLANG_DEFAULT_CXX_STDLIB=libc++ -DCLANG_DEFAULT_OBJCOPY=llvm-objcopy \
        -DLLVM_INSTALL_TOOLCHAIN_ONLY=ON \
        -DLLVM_TOOLCHAIN_TOOLS="llvm-ar;llvm-ranlib;llvm-nm;llvm-objcopy;llvm-strip;llvm-objdump;llvm-readelf;llvm-readobj;llvm-size;llvm-strings;llvm-cxxfilt"
    '';
    buildPhase = "ninja -C build -j$NIX_BUILD_CORES llvm-driver clang-resource-headers";
    # one multicall binary, a link per name LLVMDriverTools.def gives it, and clang's resource headers
    installPhase = ''
      mkdir -p $out/bin $out/lib
      cp build/bin/llvm $out/bin/llvm
      $STRIP $out/bin/llvm
      for t in $(sed -n 's/^LLVM_DRIVER_TOOL("\([^"]*\)".*/\1/p' build/tools/llvm-driver/LLVMDriverTools.def); do
        case $t in clang|clang-*|lld) ln -sfn llvm $out/bin/$t ;; dsymutil) ;; *) ln -sfn llvm $out/bin/llvm-$t ;; esac
      done
      for n in clang++ clang-cpp ld.lld ar ranlib nm objcopy objdump strip readelf size strings c++filt; do
        case $n in clang*|*lld*) ln -sfn llvm $out/bin/$n ;; *) [ -e $out/bin/llvm-$n ] && ln -sfn llvm $out/bin/$n ;; esac
      done
      cp -r build/lib/clang $out/lib/clang
      ls $out/bin $out/lib/clang/*/include | head -40
    '';
    dontFixup = true;
  };

  # a cross seed still bootstraps rust with this machine's llvm
  rustBootstrap = import ./rust.nix {
    ps = musl;
    inherit source;
    pkgs = import nixpkgs { system = buildSystem; };
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

  # hoist's sources stay out of shim's, so a builder edit leaves the toolchain under shim alone. cargo
  # still wants every workspace member's main.rs
  rustCrate =
    name: excluded:
    let
      kept =
        path: type:
        (type == "directory" && baseNameOf path != "target")
        || builtins.match ".*\\.rs" path != null
        || builtins.elem (baseNameOf path) [
          "Cargo.toml"
          "Cargo.lock"
          "README.md"
        ];
    in
    rustPlatform.buildRustPackage {
      pname = "seed-${name}";
      version = "3.0.0";
      src = builtins.path {
        path = ../../sh/shim/src;
        name = "${name}-rust-src";
        filter = path: type: !(excluded path) && kept path type;
      };
      cargoLock.lockFile = ../../sh/shim/src/Cargo.lock;
      cargoBuildFlags = [
        "-p"
        name
      ];
      auditable = false;
      doCheck = false;
      env.SHIM_STORE_DIR = builtins.storeDir;
      allowedReferences = [ ];
    };
  shim = rustCrate "shim" (
    path: builtins.match ".*/(hoist|jsem|plumb)/src/.+" path != null && baseNameOf path != "main.rs"
  );
  hoist = rustCrate "hoist" (_: false);

  nuSource = source "nushell";
  nu =
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
        pname = "seed-nushell";
        inherit (nuSource) version;
        src = nuSource.default;
        auditable = false;
        doCheck = false;
        postPatch = ''
          substituteInPlace "$cargoDepsCopy/source-registry-0/pest_consume_macros-1.1.0/src/make_parser.rs" \
            --replace-fail HashMap BTreeMap
        '';
      };

  bsdtar = musl.libarchive;
  # nu has no ln, chmod or readlink, and configure scripts want expr, tr and sed before any build.
  # defconfig leaves out toybox's pending commands, of which awk, diff, expr, gzip and tr are needed
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
  # what glibc and the GNU userland build with beyond toybox, a POSIX sh, make and the GNU text
  # tools their scripts assume. glibc ships intl/plural.y without its parser, hence bison and m4
  tools = {
    make = musl.gnumake.override { guileSupport = false; };
    gawk = musl.gawk.override { interactive = false; };
    sed = musl.gnused;
    grep = musl.gnugrep.override { pcre2 = null; };
    m4 = musl.gnum4;
    inherit (musl) bison;
  };

  # glibc's build runs python scripts, which need only the stdlib as source, without extension
  # modules or ensurepip
  python = musl.stdenv.mkDerivation {
    name = "seed-python";
    src = (source "cpython314").default;
    # every module that would want a library the seed does not carry
    preConfigure = ''
      cat > Modules/Setup.local <<EOF
      *disabled*
      _ctypes _ssl _hashlib _tkinter _curses _curses_panel _dbm _gdbm _lzma _bz2 zlib readline _sqlite3 _uuid nis ossaudiodev spwd _crypt _scproxy xxlimited xxlimited_35 _testcapi _testinternalcapi _testbuffer _testimportmultiple _testmultiphase _testsinglephase _testclinic _testexternalinspection _ctypes_test _xxtestfuzz
      EOF
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
      cd $out/lib/python3.*
      rm -rf test idlelib tkinter turtledemo ensurepip lib2to3 config-3* site-packages
      rm -rf $out/share $out/lib/pkgconfig $out/bin/idle* $out/bin/pydoc* $out/lib/libpython*.a $out/include
      find $out -name __pycache__ -prune -exec rm -rf {} +
      ${lib.optionalString (
        !cross
      ) ''$out/bin/python3 -B -c "import sys, os, re, json, subprocess, argparse"''}
    '';
    dontFixup = true;
  };

  seed = native.runCommand "seed-4-${system}" { allowedReferences = [ ]; } ''
    mkdir -p $out/bin $out/lib $out/share
    cp ${bsdtar}/bin/bsdtar ${toybox}/bin/toybox ${dash}/bin/dash $out/bin/
    ln -s dash $out/bin/sh
    ${lib.concatStringsSep "\n" (lib.mapAttrsToList (n: p: "cp ${p}/bin/${n} $out/bin/") tools)}
    cp ${tools.bison}/bin/yacc $out/bin/ 2>/dev/null || true
    cp -r ${tools.bison}/share/bison $out/share/bison
    cp -L ${python}/bin/python3 $out/bin/python3
    cp -r ${python}/lib/python3.* $out/lib/
    # its applet links, without replacing the GNU tools already there
    cp -dn ${toybox}/bin/* $out/bin/
    ln -s ld.lld $out/bin/ld  # configure scripts probe for plain `ld`
    cp -a ${llvm}/bin/. $out/bin/
    cp -a ${llvm}/lib/clang $out/lib/
    chmod -R u+w $out
    cp ${shim}/bin/shim $out/bin/shim
    ${llvm}/bin/llvm-strip $out/bin/bsdtar $out/bin/toybox $out/bin/dash $out/bin/python3 ${
      toString (map (n: "$out/bin/${n}") (builtins.attrNames tools))
    }
    find $out -type f -exec ${native.nukeReferences}/bin/nuke-refs {} +
  '';

  nar = native.runCommand "${seed.name}.nar.xz" { nativeBuildInputs = [ native.nix ]; } ''
    mkdir $out
    nix-store --dump ${seed} | ${native.xz}/bin/xz -T$NIX_BUILD_CORES -9e > $out/${seed.name}.nar.xz
    # what package.toml pins: hash of the unpacked tree (fetchurl unpack=true is recursive/NAR-hashed)
    nix-hash --type sha256 --to-sri $(nix-store --dump ${seed} | sha256sum | cut -d' ' -f1) > $out/nar-hash
  '';
in
{
  inherit
    llvm
    shim
    hoist
    nu
    bsdtar
    toybox
    dash
    python
    seed
    nar
    ;
  inherit (tools)
    make
    gawk
    sed
    grep
    m4
    bison
    ;
  inherit (rustBootstrap)
    rust
    stage0
    mrustc
    rustStages
    ;
}
