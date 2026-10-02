{ platform, when, ... }:
[
  (when platform.cross {
    after."cmake.install" = [ "llvm.host-config" ];
    cmake.defs.CROSS_TOOLCHAIN_FLAGS_NATIVE = "-DCMAKE_C_COMPILER=cc-build;-DCMAKE_CXX_COMPILER=c++-build;-DCMAKE_EXE_LINKER_FLAGS=-static;-DLLVM_BUILD_LLVM_DYLIB=ON;-DLLVM_LINK_LLVM_DYLIB=ON;-DLLVM_HOST_TRIPLE=${platform.clangTarget}";
  })
]
