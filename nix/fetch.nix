# Lock-file fetchers as dynamic derivations. `mummer fetch <kind>` reads the lock from its inputs
# and writes one builtin fetchurl per artefact, carrying the lock's own hash, plus a derivation
# that assembles them with the peanut's bsdtar. The caller gets the output of that output, so Nix
# downloads and verifies every file and no lock enters evaluation. Needs dynamic-derivations for
# `builtins.outputOf`, and a daemon with dynamic-derivations and ca-derivations.
{
  mummer,
  peanut,
  system,
  cpu,
  sevenzip ? null,
}:
let
  # a text-CA output is named after its derivation minus `.drv`, so each level of producing a
  # producer adds one
  produce =
    {
      name,
      kind,
      stages ? 1,
    }:
    inputs:
    let
      producer = derivation (
        {
          name = name + (if stages == 2 then ".drv.drv" else ".drv");
          inherit system peanut mummer;
          builder = "${mummer}/bin/mummer";
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
      first = builtins.outputOf producer.outPath "out";
    in
    if stages == 2 then builtins.outputOf first "out" else first;

  orEmpty = value: if value == null then "" else value;
  commas = builtins.concatStringsSep ",";

  tree =
    name: kind:
    {
      source,
      root ? ".",
    }:
    produce { inherit name kind; } { inherit source root; };
in
{
  # `git`: the unpacked tree of each git source by the lock's source string
  cargoVendor =
    {
      source,
      locks ? [ "Cargo.lock" ],
      git ? { },
    }:
    produce
      {
        name = "cargo-vendor";
        kind = "cargo";
      }
      {
        inherit source;
        locks = builtins.concatStringsSep "\n" locks;
        git = builtins.concatStringsSep "\n" (
          builtins.attrValues (builtins.mapAttrs (origin: tree: "${origin}=${tree}") git)
        );
      };

  # spieler writes locks for upstreams that commit none, so the lock may sit apart from the source
  npmDeps =
    {
      source,
      root ? ".",
      lockFile ? null,
    }:
    produce
      {
        name = "npm-deps";
        kind = "npm";
      }
      {
        inherit source root;
        lockFile = orEmpty lockFile;
      };

  pnpmDeps = tree "pnpm-deps" "pnpm";
  yarnDeps = tree "yarn-deps" "yarn";
  bunDeps = tree "bun-deps" "bun";
  hexDeps = tree "hex-deps" "hex";

  # jsr hashes pin metadata, the metadata pins the files, so the files come a stage later
  denoDeps =
    {
      source,
      root ? ".",
    }:
    produce {
      name = "deno-deps";
      kind = "deno";
      stages = 2;
    } { inherit source root; };

  pythonDeps =
    {
      source,
      python,
      root ? ".",
      extras ? [ ],
      groups ? [ ],
      prefer ? "wheel",
      sdist ? [ ],
      git ? [ ],
      environ ? "{}",
    }:
    produce
      {
        name = "python-deps";
        kind = "pypi";
        stages = 2;
      }
      {
        inherit
          source
          root
          prefer
          environ
          cpu
          ;
        sysSdists = commas (builtins.attrNames ((import ./bindings.nix).locked "python"));
        pythonVersion = python.version;
        extras = commas extras;
        groups = commas groups;
        sdist = commas sdist;
        git = builtins.concatStringsSep "\n" (map (entry: "${entry.name}=${entry.path}") git);
      };

  gems =
    {
      source,
      root ? ".",
      lockFile ? null,
    }:
    produce
      {
        name = "gems";
        kind = "gems";
      }
      {
        inherit source root;
        lockFile = orEmpty lockFile;
        gemPlatform = "${cpu}-linux";
      };

  # go.sum's h1: hashes are not file hashes, so the .mod and .zip hashes come from locks/go.toml
  goModules =
    {
      source,
      root ? ".",
      locks ? ../locks/go.toml,
    }:
    produce {
      name = "go-modules";
      kind = "go";
    } { inherit source root locks; };

  hackageSet =
    {
      locks ? ../locks/hackage.toml,
    }:
    produce {
      name = "hackage-set";
      kind = "hackage";
    } { inherit locks; };

  luaRocksSet =
    {
      lua,
      locks ? ../locks/luarocks.toml,
    }:
    produce
      {
        name = "luarocks-set";
        kind = "luarocks";
      }
      {
        inherit locks;
        luaVersion = lua.version;
      };

  # a directory of `{ "<relative path>" = "<contents>"; }`, laid out by `mummer assemble`
  files =
    name: contents:
    derivation {
      inherit name system peanut;
      builder = "${mummer}/bin/mummer";
      args = [ "assemble" ];
      __structuredAttrs = true;
      layout = map (to: {
        inherit to;
        write = contents.${to};
      }) (builtins.attrNames contents);
    };

  empty = builtins.path {
    path = ./empty;
    name = "empty";
  };

  # the Visual Studio licence keeps this out of the public cache
  windowsSdk =
    { manifest, arch }:
    produce
      {
        name = "windows-sdk-${arch}";
        kind = "winsdk";
      }
      {
        inherit manifest arch;
        sevenzip = "${sevenzip}";
        sevenzip_drv = sevenzip.drvPath;
      };
}
