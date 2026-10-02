# What each bootstrap recipe reads besides its stage: sources from the packages' recipe.toml,
# the C sources of the target-side runtime, patches
{
  pkg,
  source,
  prebuiltJig,
}:
let
  llvm = source "llvm";
in
{
  jig.prebuilt = prebuiltJig;
  launch.launch = pkg "launch" + "/src/launch.c";
  cc = {
    crt_interp = pkg "crt-interp" + "/src/crt_interp.c";
    reloc = builtins.path {
      name = "reloc";
      path = pkg "crt-interp" + "/src";
      filter = path: _: builtins.match "reloc.*" (baseNameOf path) != null;
    };
  };
  dlaudit.dlaudit = pkg "dlaudit" + "/src/dlaudit.c";
  llvm = {
    src = llvm;
    zstd = source "zstd";
    zlib = source "zlib";
    patches = [
      (pkg "llvm" + "/upstream-x86-vastart-stack-probe.patch")
      (pkg "llvm" + "/jig-absent-log.patch")
    ];
    targets =
      (builtins.fromTOML (builtins.readFile (pkg "llvm" + "/recipe.toml")))
      .build.cmake.defs.LLVM_TARGETS_TO_BUILD;
  };
  cmake.src = source "cmake-bootstrap";
  compiler-rt.src = llvm;
  runtimes.src = llvm;
  linux-headers.src = source "linux";
  musl.src = source "musl";
  mingw-w64.src = source "mingw-w64";
  apple-sdk.src = source "apple-sdk";
  vcruntime = { };
  glibc = {
    src = source "glibc";
    patches = map (patch: pkg "glibc" + "/${patch}") [
      "glibc-prefix-relative.patch"
      "upstream-ppc64le-clang.patch"
      "upstream-debug-after-misc.patch"
      "upstream-verneed-dst.patch"
      "glibc-unwind-origin.patch"
      "glibc-tzdir-etc-zoneinfo.patch"
      "upstream-const-generic-extension.patch"
      "glibc-loongarch-rtld-bcmp.patch"
      "upstream-ldbl128-literal.patch"
    ];
  };
}
