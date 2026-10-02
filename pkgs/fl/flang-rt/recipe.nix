{
  platform,
  pkgs,
  tools,
  toolchain,
  when,
  ...
}:
[
  {
    cmake.defs = {
      CMAKE_Fortran_COMPILER = "${tools.flang}/bin/flang";
      CMAKE_Fortran_FLAGS = "--target=${platform.clangTarget} ${builtins.head platform.march} --sysroot=${toolchain.sysroot}";
      LLVM_DEFAULT_TARGET_TRIPLE = platform.gnuTriple;
      LLVM_DIR = "${pkgs.llvm}/lib/cmake/llvm";
      LLVM_TABLEGEN_EXE = "${tools.llvm}/bin/llvm-tblgen";
      LLVM_TOOLS_BINARY_DIR = "${tools.llvm}/bin";
    };
    env.TOOLCHAIN = toolchain.outPath;
  }
  (when (!platform.cross) {
    cmake.defs = {
      LLVM_TABLEGEN_EXE = "${pkgs.llvm}/bin/llvm-tblgen";
      LLVM_TOOLS_BINARY_DIR = "${pkgs.llvm}/bin";
    };
    tools = [ pkgs.llvm ];
  })
  (when platform.cross { tools = [ "llvm" ]; })
  (when (platform.os == "macos") { cmake.defs.LLVM_DEFAULT_TARGET_TRIPLE = platform.clangTarget; })
  (when (platform.cpu == "powerpc64le") {
    cmake.defs = {
      CMAKE_CXX_FLAGS = "-mabi=ibmlongdouble";
      FOUND_LIBMF128 = false;
      HAVE_LDBL_MANT_DIG_113 = false;
    };
  })
]
