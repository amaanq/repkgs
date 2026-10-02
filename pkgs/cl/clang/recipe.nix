{
  platform,
  pkgs,
  tools,
  when,
  ...
}:
[
  {
    cmake.defs = {
      LLVM_DIR = "${pkgs.llvm}/lib/cmake/llvm";
      LLVM_TABLEGEN_EXE = "${tools.llvm}/bin/llvm-tblgen";
      LLVM_TOOLS_BINARY_DIR = "${tools.llvm}/bin";
    };
    install."bin/" = "bin/clang-tblgen${platform.ext.exe}";
  }
  (when (!platform.cross) {
    cmake.defs = {
      LLVM_TABLEGEN_EXE = "${pkgs.llvm}/bin/llvm-tblgen";
      LLVM_TOOLS_BINARY_DIR = "${pkgs.llvm}/bin";
    };
    tools = [ pkgs.llvm ];
  })
  (when platform.cross {
    cmake.defs.CLANG_TABLEGEN = "${tools.clang}/bin/clang-tblgen";
    tools = [
      "llvm"
      "clang"
    ];
  })
]
