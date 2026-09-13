{
  pkgs,
  ps,
  llvm,
}:
let
  inherit (pkgs) lib;
  host = pkgs.stdenv.hostPlatform.rust.rustcTarget;
  target = ps.stdenv.hostPlatform.rust.rustcTarget;
  cpu = pkgs.stdenv.hostPlatform.parsed.cpu.name;
  llvmTargets = {
    x86_64 = "X86";
    aarch64 = "AArch64";
    riscv64 = "RISCV";
    loongarch64 = "LoongArch";
    powerpc64le = "PowerPC";
  };
  binutils =
    pkgs.runCommand "seed-binutils"
      {
        passthru = {
          isLLVM = true;
          inherit (llvm) version;
        };
      }
      ''
        mkdir -p $out/bin
        for tool in ${llvm}/bin/*; do
          ln -s "$tool" "$out/bin/$(basename "$tool")"
        done
        ln -s ${llvm}/bin/ld.lld $out/bin/ld
      '';
  bintools = pkgs.wrapBintoolsWith { bintools = binutils; };
  cc = pkgs.wrapCCWith {
    cc = llvm;
    inherit bintools;
    isClang = true;
    gccForLibs = pkgs.stdenv.cc.cc;
    extraBuildCommands = ''
      echo '--target=${host} -B${bintools}/bin' >> $out/nix-support/cc-cflags
      echo '--start-no-unused-arguments --rtlib=libgcc --unwindlib=libgcc --end-no-unused-arguments' >> $out/nix-support/cc-cflags
      echo '-stdlib=libstdc++' >> $out/nix-support/libcxx-cxxflags
      echo '-stdlib=libstdc++' >> $out/nix-support/libcxx-ldflags
    '';
  };
  stdenv = pkgs.overrideCC pkgs.stdenv cc;
  config = lib.generators.toINIWithGlobalSection {
    mkKeyValue = key: value: "${key} = ${builtins.toJSON value}";
  };
in
{
  version,
  src,
  stage0,
  final ? false,
  stage ? 2,
  backend ? pkgs.llvmPackages_21.libllvm,
}:
let
  rust = stdenv.mkDerivation {
    pname = "seed-rust";
    inherit version src;
    nativeBuildInputs = [
      pkgs.python3Minimal
      pkgs.cmakeMinimal
      (pkgs.ninja.override {
        buildDocs = false;
        python3 = pkgs.python3Minimal;
      })
      pkgs.pkg-config
    ];
    buildInputs = [
      pkgs.openssl
      pkgs.zlib
      pkgs.curl
    ];
    dontUseCmakeConfigure = true;
    dontUpdateAutotoolsGnuConfigScripts = true;
    dontFixLibtool = true;
    stripDebugList = [ "bin" ];
    env = {
      RUSTFLAGS = "-Clinker=${cc}/bin/clang -Clinker-features=-lld -Clink-self-contained=-linker";
      OPENSSL_NO_VENDOR = "1";
    };
    configurePhase = ''
      runHook preConfigure
      cp ${
        pkgs.writeText "seed-rust-bootstrap.toml" (config {
          globalSection = {
            change-id = "ignore";
            profile = "dist";
          };
          sections = {
            llvm = {
              download-ci-llvm = false;
              targets = lib.concatStringsSep ";" (
                lib.unique (
                  [ llvmTargets.${cpu} ] ++ lib.optional final llvmTargets.${ps.stdenv.hostPlatform.parsed.cpu.name}
                )
              );
              experimental-targets = "";
              link-jobs = 2;
              use-linker = "lld";
            }
            // lib.optionalAttrs (backend != null) { link-shared = true; };
            build = {
              build = host;
              host = [ host ];
              target = [ host ] ++ lib.optional final target;
              rustc = "${stage0}/bin/rustc";
              cargo = "${stage0}/bin/cargo";
              docs = false;
              extended = true;
              tools = [ "cargo" ];
              vendor = true;
              locked-deps = true;
              optimized-compiler-builtins = false;
              description = "repkgs";
            };
            install = {
              prefix = "@out@";
              sysconfdir = "etc";
            };
            rust = {
              channel = "stable";
              download-rustc = false;
              remap-debuginfo = true;
              rpath = true;
              lld = false;
              llvm-tools = false;
              llvm-bitcode-linker = false;
              codegen-tests = false;
            };
            "target.${host}" = {
              cc = "${cc}/bin/clang";
              cxx = "${cc}/bin/clang++";
              linker = "${cc}/bin/clang";
              ar = "${llvm}/bin/llvm-ar";
              ranlib = "${llvm}/bin/llvm-ranlib";
              crt-static = false;
            }
            // lib.optionalAttrs (backend != null) {
              llvm-config = "${backend.dev}/bin/llvm-config";
            };
            dist = {
              compression-formats = [ "gz" ];
              src-tarball = false;
            };
          }
          // lib.optionalAttrs final {
            "target.${target}" = {
              cc = "${ps.stdenv.cc}/bin/${ps.stdenv.cc.targetPrefix}cc";
              cxx = "${ps.stdenv.cc}/bin/${ps.stdenv.cc.targetPrefix}c++";
              linker = "${ps.stdenv.cc}/bin/${ps.stdenv.cc.targetPrefix}cc";
              ar = "${llvm}/bin/llvm-ar";
              ranlib = "${llvm}/bin/llvm-ranlib";
              musl-root = "${ps.stdenv.cc.libc}";
              crt-static = true;
            };
          };
        })
      } bootstrap.toml
      substituteInPlace bootstrap.toml --replace-fail '@out@' "$out"
      runHook postConfigure
    '';
    buildPhase = ''
      runHook preBuild
      python3 x.py build --stage ${toString stage} -j "$NIX_BUILD_CORES"
      runHook postBuild
    '';
    installPhase = ''
      runHook preInstall
      python3 x.py install --stage ${toString stage} -j "$NIX_BUILD_CORES"
      rm -f "$out/lib/rustlib/install.log" "$out/lib/rustlib/uninstall.sh"
      runHook postInstall
    ''
    + lib.optionalString final ''
      ${llvm}/bin/llvm-strip --strip-unneeded "$out"/lib/librustc_driver-*.so
    '';
    disallowedReferences = [ stage0 ];
    passthru = {
      inherit cc;
      unwrapped = rust;
      targetPlatforms = lib.unique (
        [ pkgs.stdenv.hostPlatform.system ] ++ lib.optional final ps.stdenv.hostPlatform.system
      );
      badTargetPlatforms = [ ];
    };
    meta.platforms = [ pkgs.stdenv.hostPlatform.system ];
  };
in
rust
