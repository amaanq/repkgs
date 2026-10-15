use std::{
   ffi::OsString,
   iter::once,
   path::PathBuf,
};

use misstep::Result;
use mummer::process::text_arguments;
use pound::{
   Parse,
   ValueEnum,
};
use tightrope::key::Key;

/// compiler wrapper, compile cache client and relocation tool for motley.
#[derive(Parse)]
#[pound(name = "mummer")]
pub enum Mode {
   /// the C compiler, with the toolchain's flags and the compile cache in
   /// front.
   #[pound(
      alias = "gcc",
      alias = "clang",
      alias = "cc-build",
      alias = "gcc-build",
      alias = "clang-build"
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
   /// the C preprocessor, clang's cpp driver mode.
   Cpp,
   /// the Fortran compiler.
   #[pound(
      alias = "gfortran",
      alias = "flang",
      alias = "fortran-build",
      alias = "gfortran-build",
      alias = "flang-build"
   )]
   Fortran,
   /// rustc with the crate cache in front, `mummer rustcwrap <rustc>
   /// <args...>`.
   Rustcwrap,
   /// ghc with the module cache in front, `mummer ghcwrap <ghc> <args...>`.
   Ghcwrap,
   /// a custom build command, cached when it is a tablegen run, `mummer
   /// tblgenwrap <program> <args...>`.
   Tblgenwrap,
   /// one object or directory tree in the cache, exit 1 on a miss and 2 without
   /// a daemon.
   Cache {
      operation: CacheOperation,
      #[pound(parse = str::parse::<Key>)]
      key:       Key,
      path:      PathBuf,
   },
   /// run a program under a compiler slot, `mummer slot <program> [args...]`.
   Slot,
   /// Go's GOCACHEPROG protocol on stdin and stdout.
   Gocacheprog,
   /// autoconf header probe key, `mummer header-key <context> <warning-policy>
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
      /// the dynamic loader, whose directory's libraries stay found by name.
      #[pound(long)]
      loader:    Option<PathBuf>,
      /// a dependency's library directory, where `@rpath/` names resolve.
      #[pound(long)]
      library:   Vec<PathBuf>,
   },
   /// builder RPC to the Nix daemon.
   NixStore {
      #[pound(subcommand)]
      operation: StoreOperation,
   },

   /// one bootstrap stage from its structured attrs, `mummer bootstrap
   /// <recipe>`.
   Bootstrap { recipe: String },
   /// one lock-file producer under builder-rpc-v0, `mummer fetch <kind>`.
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
            return Ok(Self::Fortran);
         },
         "c++" | "g++" | "clang++" | "c++-build" | "g++-build" | "clang++-build" => {
            return Ok(Self::Cxx);
         },
         "cpp" => return Ok(Self::Cpp),
         "rustcwrap" => return Ok(Self::Rustcwrap),
         "ghcwrap" => return Ok(Self::Ghcwrap),
         "tblgenwrap" => return Ok(Self::Tblgenwrap),
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
