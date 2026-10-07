# rustc and cargo built from source by mrustc, the first rust compiler peanut's chain starts from.
# mrustc builds rustc and cargo, then run_rustc rebuilds rustc and std with that rustc, and the
# shipped cargo is mrustc's.
{ pkgs }:
let
  inherit (pkgs) lib stdenv;
  pin = fromTOML (builtins.readFile ./mrustc.toml);
  inherit (pin.rustc) version;
  rustcSrc = pkgs.fetchurl {
    url = "https://static.rust-lang.org/dist/rustc-${version}-src.tar.gz";
    inherit (pin.rustc) hash;
  };
  llvm = pkgs.llvmPackages_22.libllvm;
  target = stdenv.hostPlatform.rust.rustcTarget;
  outputDir = "output-${version}";
in
stdenv.mkDerivation {
  pname = "peanut-mrustc";
  inherit version;
  src = pkgs.fetchFromGitHub pin.fork;

  strictDeps = true;
  nativeBuildInputs = with pkgs; [
    cmake
    perl
    pkg-config
    python3
    which
  ];
  buildInputs = with pkgs; [
    llvm
    libffi
    zlib
    libxml2
    curl
  ];

  dontUseCmakeConfigure = true;
  dontFixLibtool = true;

  env = {
    RUSTC_VERSION = version;
    MRUSTC_TARGET_VER = lib.versions.majorMinor version;
    OUTDIR_SUF = "-${version}";
  };

  postPatch = ''
    ln -s ${rustcSrc} rustc-${version}-src.tar.gz
  '';

  buildPhase = ''
    runHook preBuild

    local flags=(
      -j$NIX_BUILD_CORES
      PARLEVEL=$NIX_BUILD_CORES
      LLVM_CONFIG=${lib.getDev llvm}/bin/llvm-config
      RUSTC_TARGET=${target}
    )

    make "''${flags[@]}" bin/mrustc
    make -C tools/minicargo "''${flags[@]}"
    make -f minicargo.mk "''${flags[@]}" LIBS
    make -f minicargo.mk "''${flags[@]}" ${outputDir}/rustc ${outputDir}/cargo
    make -C run_rustc "''${flags[@]}" ${outputDir}/prefix/bin/rustc ${outputDir}/prefix/bin/hello_world

    runHook postBuild
  '';

  doCheck = true;
  checkPhase = ''
    runHook preCheck
    run_rustc/${outputDir}/prefix/bin/hello_world | grep -Fx 'Hello, world!'
    runHook postCheck
  '';

  installPhase = ''
    runHook preInstall
    mkdir -p $out/bin $out/lib
    cp run_rustc/${outputDir}/prefix/bin/rustc_binary $out/bin/rustc
    cp ${outputDir}/cargo $out/bin/cargo
    cp -r run_rustc/${outputDir}/prefix/lib/* $out/lib/
    cp $out/lib/rustlib/${target}/lib/*.so $out/lib/
    runHook postInstall
  '';

  meta.platforms = [ "x86_64-linux" ];
}
