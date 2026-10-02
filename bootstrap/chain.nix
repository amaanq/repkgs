# libc headers -> compiler-rt -> libc -> (kernel headers) -> libc++/libunwind -> sysroot -> cc.
# The libc is a recipe run twice (headersOnly first) or `libcGiven`, an SDK that also brings
# the C++ library.
{ pkg }:
{
  platform,
  run,
  libcRecipe ? null,
  libcArgs ? { },
  libcGiven ? null,
  linuxHeaders ? null,
  extraParts ? [ ],
  ccArgs ? { },
}:
let
  given = libcGiven != null;
  libcRun = args: if given then libcGiven else run [ ] libcRecipe (libcArgs // args);
  sysrootOf =
    parts:
    run [ ] "sysroot" {
      parts = builtins.filter (part: part != null) parts;
      resource = compiler-rt;
    };
  builtinsList =
    pkg "llvm" + "/builtins-${if platform.os == "linux" then platform.cpu else platform.name}.txt";
  compiler-rt = run [ ] "compiler-rt" (
    {
      libcHeaders = libcRun { headersOnly = "1"; };
      list = builtinsList;
    }
    // (if linuxHeaders == null then { } else { inherit linuxHeaders; })
  );
  libc = libcRun { inherit compiler-rt; };
  base = [
    libc
    linuxHeaders
  ]
  ++ extraParts;
  runtimes = if given then null else run [ ] "runtimes" { sysroot = sysrootOf base; };
  sysroot = sysrootOf (base ++ [ runtimes ]);
in
{
  inherit
    platform
    compiler-rt
    libc
    runtimes
    sysroot
    ;
  cc = run [ ] "cc" ({ inherit sysroot; } // ccArgs);
}
