# One rustc and cargo release built from source by the release before it, with the peanut's
# llvm as the C toolchain. rust.nix folds the chain from mrustc to the shipped compiler, which is
# `final` and also targets the musl set `ps`.
{
  pkgs,
  ps,
  llvm,
}:
let
  inherit (pkgs) lib;
  host = pkgs.stdenv.hostPlatform.rust.rustcTarget;
  target = ps.stdenv.hostPlatform.rust.rustcTarget;
  backends = {
    x86_64 = "X86";
    aarch64 = "AArch64";
    riscv64 = "RISCV";
    loongarch64 = "LoongArch";
    powerpc64le = "PowerPC";
  };
  backendOf = platform: backends.${platform.parsed.cpu.name};

  binutils =
    pkgs.runCommand "peanut-binutils"
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
  # libgcc and libstdc++ come from nixpkgs' gcc, since the peanut llvm ships no runtimes
  cc = pkgs.wrapCCWith {
    cc = llvm;
    inherit bintools;
    isClang = true;
    gccForLibs = pkgs.stdenv.cc.cc;
    extraBuildCommands = ''
      echo "--target=${host} -B${bintools}/bin" >> $out/nix-support/cc-cflags
      echo "--start-no-unused-arguments --rtlib=libgcc --unwindlib=libgcc --end-no-unused-arguments" >> $out/nix-support/cc-cflags
      echo "-stdlib=libstdc++" >> $out/nix-support/libcxx-cxxflags
      echo "-stdlib=libstdc++" >> $out/nix-support/libcxx-ldflags
    '';
  };
  stdenv = pkgs.overrideCC pkgs.stdenv cc;

  # x.py reads TOML, and JSON strings, booleans and lists are TOML values too
  toml =
    sections:
    lib.concatStringsSep "\n" (
      lib.mapAttrsToList (
        name: keys:
        lib.concatStringsSep "\n" (
          lib.optional (name != "") "[${name}]"
          ++ lib.mapAttrsToList (key: value: "${key} = ${builtins.toJSON value}") keys
        )
        + "\n"
      ) sections
    );
in
{
  version,
  src,
  stage0,
  final ? false,
  stage ? 2,
  # null builds the llvm in rustc's own tree
  backend ? pkgs.llvmPackages_21.libllvm,
}:
let
  config = pkgs.writeText "peanut-rust-bootstrap.toml" (
    toml (
      {
        "" = {
          change-id = "ignore";
          profile = "dist";
        };
        llvm = {
          download-ci-llvm = false;
          targets = lib.concatStringsSep ";" (
            lib.unique (
              [ (backendOf pkgs.stdenv.hostPlatform) ] ++ lib.optional final (backendOf ps.stdenv.hostPlatform)
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
          tools = [
            "cargo"
            "rustdoc"
          ];
          vendor = true;
          locked-deps = true;
          optimized-compiler-builtins = false;
          description = "motley";
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
        // lib.optionalAttrs (backend != null) { llvm-config = "${lib.getDev backend}/bin/llvm-config"; };
        dist = {
          compression-formats = [ "gz" ];
          src-tarball = false;
        };
      }
      // lib.optionalAttrs final {
        "target.${target}" =
          let
            inherit (ps.stdenv) cc;
            driver = "${cc}/bin/${cc.targetPrefix}";
          in
          {
            cc = "${driver}cc";
            cxx = "${driver}c++";
            linker = "${driver}cc";
            ar = "${llvm}/bin/llvm-ar";
            ranlib = "${llvm}/bin/llvm-ranlib";
            musl-root = "${cc.libc}";
            crt-static = true;
          };
      }
    )
  );
in
stdenv.mkDerivation (self: {
  pname = "peanut-rust";
  inherit version src;

  nativeBuildInputs = with pkgs; [
    python3Minimal
    cmakeMinimal
    (ninja.override {
      buildDocs = false;
      python3 = python3Minimal;
    })
    pkg-config
  ];
  buildInputs = with pkgs; [
    openssl
    zlib
    curl
  ];

  dontUseCmakeConfigure = true;
  dontUpdateAutotoolsGnuConfigScripts = true;
  dontFixLibtool = true;
  stripDebugList = [ "bin" ];

  RUSTFLAGS = "-Clinker=${cc}/bin/clang -Clinker-features=-lld -Clink-self-contained=-linker";
  OPENSSL_NO_VENDOR = 1;
  # the bootstrap compiler builds this one and must not outlive it
  disallowedReferences = [ stage0 ];

  configurePhase = ''
    runHook preConfigure
    substitute ${config} bootstrap.toml --replace-fail @out@ "$out"
    runHook postConfigure
  '';
  buildPhase = ''
    runHook preBuild
    python3 x.py build --stage ${toString stage} -j $NIX_BUILD_CORES
    runHook postBuild
  '';
  installPhase = ''
    runHook preInstall
    python3 x.py install --stage ${toString stage} -j $NIX_BUILD_CORES
    rm $out/lib/rustlib/install.log $out/lib/rustlib/uninstall.sh
  ''
  + lib.optionalString final ''
    ${llvm}/bin/llvm-strip --strip-unneeded $out/lib/librustc_driver-*.so
  ''
  + ''
    runHook postInstall
  '';

  passthru = {
    inherit cc;
    unwrapped = self.finalPackage;
    targetPlatforms = lib.unique (
      [ pkgs.stdenv.hostPlatform.system ] ++ lib.optional final ps.stdenv.hostPlatform.system
    );
    badTargetPlatforms = [ ];
  };
  meta.platforms = [ pkgs.stdenv.hostPlatform.system ];
})
