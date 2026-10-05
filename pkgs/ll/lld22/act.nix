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
      LLVM_DIR = "${pkgs.llvm22}/lib/cmake/llvm";
      LLVM_TABLEGEN_EXE = "${tools.llvm22}/bin/llvm-tblgen";
      LLVM_TOOLS_BINARY_DIR = "${tools.llvm22}/bin";
    };
  }
  (when (!platform.cross) {
    cmake.defs = {
      LLVM_TABLEGEN_EXE = "${pkgs.llvm22}/bin/llvm-tblgen";
      LLVM_TOOLS_BINARY_DIR = "${pkgs.llvm22}/bin";
    };
    tools = [ pkgs.llvm22 ];
  })
  (when platform.cross { tools = [ "llvm22" ]; })
]
