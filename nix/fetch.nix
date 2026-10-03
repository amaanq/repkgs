# Dependency fetchers driven by lock files, the upstream tarballs come from recipe.toml.
#
# Each is a dynamic derivation. A producer, `shim fetch <kind>` under builder-rpc-v0, reads the lock
# out of its inputs and writes one builtin:fetchurl derivation per artefact with the lock's own
# hash, plus one that assembles them (`shim assemble`, with the crib's bsdtar). The producer's
# output is that .drv and callers get `builtins.outputOf` it, so Nix downloads, caches and checks
# every file and no lock is copied here. The daemon needs dynamic-derivations and ca-derivations.
{
  shim,
  crib,
  system,
  cpu,
  sevenzip ? null,
}:
let
  inherit (builtins) concatStringsSep genList outputOf;

  # a text-CA output is named after its derivation minus ".drv", so a producer that writes another
  # producer carries one ".drv" per stage
  dynamic =
    name: kind: stages: inputs:
    let
      producer = derivation (
        {
          name = name + concatStringsSep "" (genList (_: ".drv") stages);
          inherit system crib shim;
          builder = "${shim}/bin/shim";
          args = [
            "fetch"
            kind
          ];
          requiredSystemFeatures = [ "builder-rpc-v0" ];
          preferLocalBuild = true;
          __contentAddressed = true;
          outputHashMode = "text";
          outputHashAlgo = "sha256";
        }
        // inputs
      );
      once = outputOf producer.outPath "out";
    in
    if stages == 1 then once else outputOf once "out";

  inTree =
    name: kind: stages:
    {
      source,
      root ? ".",
    }:
    dynamic name kind stages { inherit source root; };
  # upstreams that commit no lock get one from `plumb lock`
  lockOrNone = lockFile: if lockFile == null then "" else lockFile;
in
{
  # Cargo.lock to a [source.vendored-sources] directory, crates unpacked with .cargo-checksum.json
  cargoVendor = { source }: dynamic "cargo-vendor" "cargo" 1 { inherit source; };

  # package-lock.json v2 or v3 to a copy whose `resolved` entries are file: store paths, which
  # `npm ci --offline` then checks itself
  npmDeps =
    {
      source,
      root ? ".",
      lockFile ? null,
    }:
    dynamic "npm-deps" "npm" 1 {
      inherit source root;
      lockFile = lockOrNone lockFile;
    };

  # pnpm-lock.yaml v9 to registry/<url path>, a file:// registry
  pnpmDeps = inTree "pnpm-deps" "pnpm" 1;
  # classic yarn.lock to an offline mirror
  yarnDeps = inTree "yarn-deps" "yarn" 1;
  # bun.lock to p/<name>@<version>/ unpacked and an index.json
  bunDeps = inTree "bun-deps" "bun" 1;
  # deno.lock v4 or later to a $DENO_DIR. jsr hashes pin metadata that in turn pins the files, so
  # a second stage (deno-files) fetches those
  denoDeps = inTree "deno-deps" "deno" 2;
  # mix.lock or rebar.lock to packages/hexpm/<name>-<version>.tar
  hexDeps = inTree "hex-deps" "hex" 1;

  # uv.lock to dist/, vendor/ and plan.json for the project's runtime closure
  pythonDeps =
    {
      source,
      # the interpreter the wheels must fit
      python,
      root ? ".",
      extras ? [ ],
      # PEP 735 groups of the project
      groups ? [ ],
      # "wheel" or "sdist", where the lock has both
      prefer ? "wheel",
      # built from sdist whatever `prefer` says, besides sys-libs.nu's and tool.uv.no-binary-package
      sdist ? [ ],
      # { name, path } per git source of the lock, fetched by nix/python.nix
      git ? [ ],
      # PEP 508 marker values as JSON
      environ ? "{}",
    }:
    dynamic "python-deps" "pypi" 2 {
      inherit
        source
        root
        cpu
        prefer
        environ
        ;
      sysLibs = ../builder/sys-libs.json;
      pythonVersion = python.version;
      extras = concatStringsSep "," extras;
      groups = concatStringsSep "," groups;
      sdist = concatStringsSep "," sdist;
      git = concatStringsSep "\n" (map (entry: "${entry.name}=${entry.path}") git);
    };

  # a Gemfile.lock with CHECKSUMS (Bundler 2.6 on) to vendor/cache/*.gem and the lock, for
  # `bundle install --local`
  gems =
    {
      source,
      root ? ".",
      lockFile ? null,
    }:
    dynamic "gems" "gems" 1 {
      inherit source root;
      lockFile = lockOrNone lockFile;
      gemPlatform = "${cpu}-linux";
    };

  # go.sum to a GOPROXY=file:// tree. Its h1: hashes are no file hashes, so the .mod and .zip
  # sha256s come from locks/go.toml, and only this package's share lands in the output
  goModules =
    {
      source,
      root ? ".",
      locks ? ../locks/go.toml,
    }:
    dynamic "go-modules" "go" 1 { inherit source root locks; };

  # locks/hackage.toml to one file+noindex repository for every cabal package
  hackageSet =
    {
      locks ? ../locks/hackage.toml,
    }:
    dynamic "hackage-set" "hackage" 1 { inherit locks; };

  # locks/luarocks.toml to a rocks server directory, .src.rock files and a manifest
  luaRocksSet =
    {
      lua,
      locks ? ../locks/luarocks.toml,
    }:
    dynamic "luarocks-set" "luarocks" 1 {
      inherit locks;
      luaVersion = lua.version;
    };

  empty = builtins.path {
    path = ./empty;
    name = "empty";
  };

  # VisualStudio.vsman to crt/ and sdk/, the MSVC CRT and STL, the UCRT and the Windows SDK for one
  # cpu. The Visual Studio license keeps it out of the public cache
  windowsSdk =
    { manifest, arch }:
    dynamic "windows-sdk-${arch}" "winsdk" 1 {
      inherit manifest arch;
      sevenzip = "${sevenzip}";
      sevenzip_drv = sevenzip.drvPath;
    };
}
