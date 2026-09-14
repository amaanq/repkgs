use std::{
   ffi::OsString,
   iter::once,
   path::PathBuf,
};

use eyre::{
   Result,
   eyre,
};
use pound::{
   Parse,
   ValueEnum,
};

/// compiler wrapper, compile cache client and output tree tool for repkgs.
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
      dest:      Option<String>,
      #[pound(long)]
      deny:      Vec<String>,
   },
   /// builder RPC to the Nix daemon.
   NixStore {
      #[pound(subcommand)]
      operation: StoreOperation,
   },
   /// point #!/usr/bin/env scripts at the build's env, or back with --undo.
   Shebangs {
      directory: PathBuf,
      #[pound(long)]
      undo:      bool,
   },
   /// each store path's exports with defaults filled in, as JSON.
   Exports { roots: Vec<String> },
   /// the dependency closure over propagate, as JSON.
   Deps { roots: Vec<String> },
   /// write exports.json from the spec on stdin.
   WriteExports { output: PathBuf },
   /// drop docs and junk, unpack man pages, print the inventory as JSON.
   Prune { output: PathBuf },
   /// make .pc files, foo-config scripts and links relative to the store path,
   /// with --macho cmake's exported sonames too.
   Relativize {
      prefix: String,
      dest:   String,
      #[pound(long)]
      macho:  bool,
   },
   /// move the DWARF of what jig linked into the debug output, by build-id.
   SplitDebug { output: PathBuf, debug: PathBuf },
   /// everything before the first build phase, prints { env, cwd, ctx } as
   /// JSON and writes ctx.json under the build top.
   Prepare {
      #[pound(long)]
      from_tree: Option<PathBuf>,
   },
   /// everything after the last build phase, up to the version check and the
   /// cache summary.
   Finish {
      #[pound(long)]
      keep_tree: bool,
      #[pound(long)]
      tests:     bool,
   },
   /// content identity of each store path, memoized in jigd.
   Identify { roots: Vec<String> },
   /// configure-time probe results shared through jigd.
   Probe {
      #[pound(subcommand)]
      operation: ProbeOperation,
   },
   /// the build systems jig implements, make, autotools and cmake.
   System {
      #[pound(subcommand)]
      operation: SystemOperation,
   },
   /// the build environment from the structured attrs, as JSON for load-env.
   Env {
      out:   PathBuf,
      #[pound(long)]
      cache: bool,
   },
   /// give upstream ELFs our interpreter, the relocation stub and a padded
   /// RUNPATH over the dependencies.
   Implant {
      directory: PathBuf,
      #[pound(long)]
      interp:    PathBuf,
      #[pound(long)]
      stub:      Option<PathBuf>,
      deps:      Vec<String>,
   },
   /// put a launch record and a symlink to launch in front of every bin/
   /// script and wrapped binary.
   Launchers {
      output: PathBuf,
      #[pound(long)]
      launch: PathBuf,
      #[pound(long)]
      interp: PathBuf,
      #[pound(long)]
      ldso:   bool,
      deps:   Vec<String>,
   },
}

impl Mode {
   pub fn from_arguments(name: &str, args: &[OsString]) -> Result<Self> {
      match name {
         "cc" | "gcc" | "clang" | "cc-build" | "gcc-build" | "clang-build" => return Ok(Self::Cc),
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

pub fn text_arguments(args: &[OsString]) -> Result<Vec<String>> {
   args
      .iter()
      .map(|arg| {
         arg.to_str()
            .map(str::to_owned)
            .ok_or_else(|| eyre!("argument {arg:?} is not UTF-8"))
      })
      .collect()
}

#[derive(Parse)]
pub enum ProbeOperation {
   /// the key for a kind of probe over its scripts and this build's inputs.
   Key {
      kind:    String,
      scripts: Vec<String>,
      #[pound(long)]
      extra:   Option<String>,
      #[pound(long)]
      no_out:  bool,
   },
   /// fetch a stored file, exit 1 when there is none.
   Restore { key: String, file: PathBuf },
   /// store a file for the next build with the same key.
   Store { key: String, file: PathBuf },
   /// fetch a stored directory tree, exit 1 when there is none.
   RestoreDir { key: String, dir: PathBuf },
   /// store a directory tree.
   StoreDir { key: String, dir: PathBuf },
   /// a CMakeCache.txt's probe results as a -C init script.
   CmakeInit { cache: PathBuf },
}

#[derive(Parse)]
pub enum SystemOperation {
   /// the environment for the phases, as JSON, after creating the working
   /// directory.
   Setup { system: String },
   /// the working directory.
   Workdir { system: String },
   /// one phase, in the working directory.
   Phase { system: String, verb: String },
}
