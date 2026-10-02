# `(stage { … }) extraTools recipe args`: one recipe run, a derivation whose builder is
# `jig bootstrap <recipe>`. PATH is extraTools (jig's clang shim first once it exists),
# the stage's tools, the seed. JIG_CC is the clang behind the shim
{
  system,
  seed,
  jig,
  buildTriple,
  inputs,
}:
let
  # `derivation` minus the per-output and override plumbing: these are single-output leaves
  leaf =
    attrs:
    let
      strict = builtins.derivationStrict attrs;
    in
    attrs
    // {
      type = "derivation";
      outputName = "out";
      outPath = strict.out;
      inherit (strict) drvPath;
    };
  shared = {
    inherit system seed;
    __structuredAttrs = true;
    __contentAddressed = true;
    outputHashMode = "recursive";
    outputHashAlgo = "sha256";
    outputs = [ "out" ];
    builder = "${jig}/bin/jig";
    JIG_STORE_IDENTITY = "content";
    M4 = "m4";
    BISON_PKGDATADIR = "${seed}/share/bison";
    CONFIG_SITE = "${../nix/config.site}";
  };
  binPath = tools: builtins.concatStringsSep ":" (map (tool: "${tool}/bin") tools);
in
{
  platform,
  clang ? seed, # whose bin/clang compiles, and what the compiler driver uses
  tools ? [ ],
  suffix ? "-${platform.name}",
}:
let
  facts = shared // {
    inherit (platform)
      clangTarget
      cpu
      libc
      os
      binfmt
      interp
      ;
    karch = platform.names.kernel;
    inherit buildTriple;
    site_libc = platform.libc;
    flags = toString platform.march;
    llvm = clang;
    JIG_CC = "${clang}/bin/clang";
  };
  basePath = binPath (tools ++ [ seed ]);
in
extraTools: recipe: args:
leaf (
  facts
  // {
    name = recipe + (if args ? headersOnly then "-headers" else "") + suffix;
    args = [
      "bootstrap"
      recipe
    ];
    PATH = if extraTools == [ ] then basePath else "${binPath extraTools}:${basePath}";
  }
  // (inputs.${recipe} or { })
  // args
)
