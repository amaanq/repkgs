{ pkgs, ... }:
[
  {
    cmake.defs = {
      CLANG_DIR = "${pkgs.clang}/lib/cmake/clang";
      LLVM_DIR = "${pkgs.llvm}/lib/cmake/llvm";
      LLVM_TABLEGEN_EXE = "${pkgs.llvm}/bin/llvm-tblgen";
      LLVM_TOOLS_BINARY_DIR = "${pkgs.llvm}/bin";
      MLIR_DIR = "${pkgs.mlir}/lib/cmake/mlir";
      MLIR_TABLEGEN_EXE = "${pkgs.mlir}/bin/mlir-tblgen";
    };
  }
]
