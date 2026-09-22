# The seed: every build-machine binary needed before the set can build its own userland, static
# musl, built once with nixpkgs (later: by the package set itself). `system` is where the seed
# runs, `buildSystem` where it is built: static musl is a cross build to nixpkgs either way.
#   ./upload.nu builds -A nar per system, uploads to the GitHub release and rewrites ./sources.toml
{
  nixpkgs ? import ../../../nix/nixpkgs.nix,
  system ? builtins.currentSystem,
  buildSystem ? builtins.currentSystem,
}:
let
  cross = system != buildSystem;
  cpu = builtins.head (builtins.split "-" system);
  ps = import nixpkgs {
    localSystem = buildSystem;
    crossSystem = {
      config = "${cpu}-unknown-linux-musl";
      isStatic = true;
    };
  };
  pkgs = ps.buildPackages;
  # The LLVM build must precede Rust and Nu, including the tools that unpack its sources.
  source =
    name:
    let
      manifest = fromTOML (
        builtins.readFile (../.. + "/${builtins.substring 0 2 name}/${name}/sources.toml")
      );
      fetch =
        key:
        let
          archive = builtins.head (builtins.filter (entry: entry.key == key) manifest.source);
        in
        (pkgs.fetchzip.override { withUnzip = false; }) {
          name = "${name}-${manifest.pin.version}-${if key == "default" then "source" else key}";
          url =
            builtins.replaceStrings
              [ "{version}" "{tag}" ]
              [
                manifest.pin.version
                (manifest.pin.tag or manifest.pin.version)
              ]
              archive.url;
          inherit (archive) hash;
        };
    in
    {
      inherit (manifest.pin) version;
      inherit fetch;
      default = fetch "default";
    };
  llvmSource = source "llvm";
  # the seed compiles stage0 and stage1 for the build machine only (bootstrap/default.nix)
  targets =
    {
      x86_64 = "X86";
      aarch64 = "AArch64";
      riscv64 = "RISCV";
      loongarch64 = "LoongArch";
      powerpc64le = "PowerPC";
    }
    .${cpu};
  triple = ps.stdenv.hostPlatform.config;

  llvm = ps.stdenv.mkDerivation {
    pname = "seed-llvm";
    inherit (llvmSource) version;
    src = llvmSource.default;
    patches = [ ../../ll/llvm/upstream-x86-vastart-stack-probe.patch ];
    nativeBuildInputs = [
      pkgs.cmakeMinimal
      (pkgs.ninja.override {
        buildDocs = false;
        python3 = pkgs.python3Minimal;
      })
      pkgs.python3Minimal
    ];
    buildInputs = [
      ps.zlib
      ps.zstd
    ];
    dontUseCmakeConfigure = true;
    # a musl-static binary for the build machine runs there: configured as a native build. For
    # another cpu llvm builds its tblgen tools in a NATIVE sub-tree with the build machine's compiler
    configurePhase = ''
      cmake -S llvm -B build -G Ninja \
        -DCMAKE_BUILD_TYPE=Release -DCMAKE_INSTALL_PREFIX=$out \
        -DCMAKE_C_COMPILER=$CC -DCMAKE_CXX_COMPILER=$CXX -DCMAKE_AR=$(command -v $AR) -DCMAKE_RANLIB=$(command -v $RANLIB) \
        ${pkgs.lib.optionalString cross "-DCMAKE_SYSTEM_NAME=Linux -DCMAKE_SYSTEM_PROCESSOR=${cpu} -DCROSS_TOOLCHAIN_FLAGS_NATIVE='-DCMAKE_C_COMPILER=${pkgs.stdenv.cc}/bin/cc;-DCMAKE_CXX_COMPILER=${pkgs.stdenv.cc}/bin/c++'"} \
        -DLLVM_ENABLE_PROJECTS="clang;lld" \
        -DLLVM_TARGETS_TO_BUILD="${targets}" \
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
    # the multicall binary + the names it answers to (from the generated .def) + clang's resource headers
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

  rustBootstrap = import ./rust.nix {
    inherit ps source;
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
  nuSource = source "nushell";
  rustPlatform = ps.makeRustPlatform {
    rustc = rustBootstrap.rust;
    cargo = rustBootstrap.rust;
  };
  # jib's sources stay out of jig's so that editing the builder side does not rebuild the
  # toolchain and everything under it. cargo still wants the workspace member to have its main.rs
  rustCrate =
    name: skipped:
    rustPlatform.buildRustPackage {
      pname = "seed-${name}";
      version = "3.0.0";
      src = builtins.path {
        path = ../../ji/jig/src;
        name = "${name}-rust-src";
        filter =
          path: type:
          !(skipped path)
          && (
            (type == "directory" && baseNameOf path != "target")
            || builtins.match ".*\\.rs" path != null
            || builtins.elem (baseNameOf path) [
              "Cargo.toml"
              "Cargo.lock"
              "README.md"
            ]
          );
      };
      cargoLock.lockFile = ../../ji/jig/src/Cargo.lock;
      cargoBuildFlags = [
        "-p"
        name
      ];
      auditable = false;
      doCheck = false;
      env.JIG_STORE_DIR = builtins.storeDir;
      allowedReferences = [ ];
    };
  jig = rustCrate "jig" (
    path: builtins.match ".*/(jib|jsem|uptrack)/src/.+" path != null && baseNameOf path != "main.rs"
  );
  jib = rustCrate "jib" (_path: false);
  nu =
    (ps.nushell.override {
      inherit rustPlatform;
      withDefaultFeatures = false;
      additionalFeatures = _features: [
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
  bsdtar = ps.libarchive;
  # nu has no ln/chmod/readlink and configure scripts need expr/tr/sed/... before anything is built
  toybox = ps.toybox.overrideAttrs (_o: {
    configurePhase = "cp ${../../to/toybox/toybox.config} .config; chmod +w .config; make oldconfig </dev/null >/dev/null";
  });

  # what glibc's and the GNU userland's own builds need beyond toybox: a POSIX sh, make, and the
  # GNU text tools their scripts are written for. bison+m4 because glibc ships only intl/plural.y
  inherit (ps) dash;
  tools = {
    make = ps.gnumake.override { guileSupport = false; };
    gawk = ps.gawk.override { interactive = false; };
    sed = ps.gnused;
    grep = ps.gnugrep.override { pcre2 = null; };
    m4 = ps.gnum4;
    inherit (ps) bison;
  };
  # glibc's build runs python scripts. No extension modules, no ensurepip: the stdlib as source
  python = ps.stdenv.mkDerivation {
    name = "seed-python";
    src = (source "cpython314").default;
    # everything that would want a library we do not ship
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
      "--with-build-python=${pkgs.python314}/bin/python3"
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
      ${pkgs.lib.optionalString (
        !cross
      ) ''$out/bin/python3 -B -c "import sys, os, re, json, subprocess, argparse"''}
    '';
    dontFixup = true;
  };

  seed = pkgs.runCommand "seed-4-${system}" { allowedReferences = [ ]; } ''
    mkdir -p $out/bin $out/lib $out/share
    cp ${bsdtar}/bin/bsdtar ${toybox}/bin/toybox ${dash}/bin/dash $out/bin/
    ln -s dash $out/bin/sh
    ${pkgs.lib.concatStringsSep "\n" (
      pkgs.lib.mapAttrsToList (n: p: "cp ${p}/bin/${n} $out/bin/") tools
    )}
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
    cp ${jig}/bin/jig $out/bin/jig
    ${llvm}/bin/llvm-strip $out/bin/bsdtar $out/bin/toybox $out/bin/dash $out/bin/python3 ${
      toString (map (n: "$out/bin/${n}") (builtins.attrNames tools))
    }
    find $out -type f -exec ${pkgs.nukeReferences}/bin/nuke-refs {} +
  '';

  nar = pkgs.runCommand "${seed.name}.nar.xz" { nativeBuildInputs = [ pkgs.nix ]; } ''
    mkdir $out
    nix-store --dump ${seed} | ${pkgs.xz}/bin/xz -T$NIX_BUILD_CORES -9e > $out/${seed.name}.nar.xz
    # what sources.toml pins: hash of the unpacked tree (fetchurl unpack=true is recursive/NAR-hashed)
    nix-hash --type sha256 --to-sri $(nix-store --dump ${seed} | sha256sum | cut -d' ' -f1) > $out/nar-hash
  '';
in
{
  inherit
    llvm
    jig
    jib
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
