use std::{
   ffi::OsString,
   iter::once,
   path::PathBuf,
};

use eyre::Result;
use jig::process::text_arguments;
use pound::{
   Parse,
   ValueEnum,
};

/// compiler wrapper, compile cache client and relocation tool for repkgs.
#[derive(Parse)]
#[pound(name = "jig")]
pub enum Mode {
   /// the C compiler, with the toolchain's flags and the compile cache in
   /// front.
   #[pound(
      alias = "gcc",
      alias = "clang",
      alias = "cc-build",
      alias = "gcc-build",
      alias = "clang-build",
      alias = "fortran",
      alias = "gfortran",
      alias = "flang",
      alias = "fortran-build",
      alias = "gfortran-build",
      alias = "flang-build"
   )]
   Cc,
   /// the C++ compiler.
   #[pound(
      name = "c++",
      alias = "g++",
      alias = "clang++",
      alias = "c++-build",
      alias = "g++-build",
      alias = "clang++-build"
   )]
   Cxx,
   /// rustc with the crate cache in front, `jig rustcwrap <rustc> <args...>`.
   Rustcwrap,
   /// one object or directory tree in the cache, exit 1 on a miss and 2 without
   /// a daemon.
   Cache {
      operation: CacheOperation,
      key:       String,
      path:      PathBuf,
   },
   /// run a program under a compiler slot, `jig slot <program> [args...]`.
   Slot,
   /// Go's GOCACHEPROG protocol on stdin and stdout.
   Gocacheprog,
   /// autoconf header probe key, `jig header-key <context> <warning-policy>
   /// <compiler> <args...>`.
   HeaderKey,
   /// make an output tree relocatable, ELF and Mach-O.
   RelocFixup {
      directory: PathBuf,
      #[pound(long)]
      dest:      Option<PathBuf>,
      #[pound(long)]
      sdk:       Option<PathBuf>,
      #[pound(long)]
      deny:      Vec<String>,
   },
   /// builder RPC to the Nix daemon.
   NixStore {
      #[pound(subcommand)]
      operation: StoreOperation,
   },

   /// content identity of each store path, memoized in jigd.
   Identify { roots: Vec<String> },
   /// one bootstrap stage from its structured attrs, `jig bootstrap <recipe>`.
   Bootstrap { recipe: String },
   /// one lock-file producer under builder-rpc-v0, `jig fetch <kind>`.
   Fetch { kind: String },
   /// lay a collecting derivation out from its structured attrs.
   Assemble,
   /// unpack the Windows SDK payloads of a collecting derivation.
   WinsdkAssemble,
   /// the first of `$urls` that answers, unpacked into `$out`.
   Unpack,
}

impl Mode {
   pub fn from_arguments(name: &str, args: &[OsString]) -> Result<Self> {
      match name {
         "cc" | "gcc" | "clang" | "cc-build" | "gcc-build" | "clang-build" => return Ok(Self::Cc),
         fortran if fortran.contains("fortran") || fortran.contains("flang") => {
            return Ok(Self::Cc);
         },
         "c++" | "g++" | "clang++" | "c++-build" | "g++-build" | "clang++-build" => {
            return Ok(Self::Cxx);
         },
         "rustcwrap" => return Ok(Self::Rustcwrap),
         "slot" => return Ok(Self::Slot),
         "header-key" => return Ok(Self::HeaderKey),
         _ => {},
      }

      let text = text_arguments(args)?;
      Ok(Self::try_parse_from(
         once(name).chain(text.iter().map(String::as_str)),
      )?)
   }
}

#[derive(Clone, Copy, ValueEnum)]
pub enum CacheOperation {
   Get,
   Put,
   GetDir,
   PutDir,
}

#[derive(Parse)]
pub enum StoreOperation {
   /// add stdin as a text store path with the given references, print its path.
   AddText {
      name:       String,
      references: Vec<String>,
   },
   /// add a derivation given as JSON on stdin, print its path.
   AddDrv {
      name:       String,
      references: Vec<String>,
   },
   /// add one builtin:fetchurl derivation per JSON record on stdin.
   Fetchurls,
   /// register an output of a derivation.
   Submit { path: String, output: String },
}
