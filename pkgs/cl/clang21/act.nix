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
      LLVM_DIR = "${pkgs.llvm21}/lib/cmake/llvm";
      LLVM_TABLEGEN_EXE = "${tools.llvm21}/bin/llvm-tblgen";
      LLVM_TOOLS_BINARY_DIR = "${tools.llvm21}/bin";
    };
    install."bin/" = "bin/clang-tblgen${platform.ext.exe}";
  }
  (when (!platform.cross) {
    cmake.defs = {
      LLVM_TABLEGEN_EXE = "${pkgs.llvm21}/bin/llvm-tblgen";
      LLVM_TOOLS_BINARY_DIR = "${pkgs.llvm21}/bin";
    };
    tools = [ pkgs.llvm21 ];
  })
  (when platform.cross {
    cmake.defs.CLANG_TABLEGEN = "${tools.clang21}/bin/clang-tblgen";
    tools = [
      "llvm21"
      "clang21"
    ];
  })
]
