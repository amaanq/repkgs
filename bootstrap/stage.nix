# `(stage { … }) extraTools recipe args`: one recipe run, a derivation whose builder is
# `mummer bootstrap <recipe>`. PATH is extraTools (mummer's clang mummer first once it exists),
# the stage's tools, the peanut. MUMMER_CC is the clang behind the mummer
{
  system,
  peanut,
  mummer,
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
    inherit system peanut;
    __structuredAttrs = true;
    __contentAddressed = true;
    outputHashMode = "recursive";
    outputHashAlgo = "sha256";
    outputs = [ "out" ];
    builder = "${mummer}/bin/mummer";
    MUMMER_STORE_IDENTITY = "content";
    M4 = "m4";
    BISON_PKGDATADIR = "${peanut}/share/bison";
    CONFIG_SITE = "${../nix/config.site}";
  };
  binPath = tools: builtins.concatStringsSep ":" (map (tool: "${tool}/bin") tools);
in
{
  platform,
  clang ? peanut, # whose bin/clang compiles, and what the compiler driver uses
  tools ? [ ],
  role ? platform.name,
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
    MUMMER_CC = "${clang}/bin/clang";
  };
  basePath = binPath (tools ++ [ peanut ]);
in
extraTools: recipe: args:
leaf (
  facts
  // {
    name = recipe + (if args ? headersOnly then "-headers" else "") + "-${role}";
    args = [
      "bootstrap"
      recipe
    ];
    PATH = if extraTools == [ ] then basePath else "${binPath extraTools}:${basePath}";
  }
  // (inputs.${recipe} or { })
  // args
)
