use std::{
   collections::BTreeMap,
   path::PathBuf,
};

use mummer::syslibs::Ecosystem;
use pound::Parse;
use serde_json::Value;
use tightrope::key::Key;

/// the build script executor for motley, everything between the structured
/// attrs and the finished output tree.
#[derive(Parse)]
#[pound(name = "bigtop")]
pub enum Mode {
   Schema {
      #[pound(long)]
      check: Option<PathBuf>,
   },
   /// the whole package build from the structured attrs, the derivation's
   /// builder.
   Build {
      #[pound(long)]
      tests:     bool,
      #[pound(long)]
      from_tree: Option<PathBuf>,
   },
   Dev {
      #[pound(subcommand)]
      operation: DevOperation,
   },
   /// resolve shebang interpreters from PATH, or restore originals with --undo.
   Shebangs {
      directory: PathBuf,
      #[pound(long)]
      undo:      bool,
   },
   /// each store path's exports with defaults filled in, as JSON.
   Exports {
      roots: Vec<String>,
   },
   /// the dependency closure over propagate, as JSON.
   Deps {
      roots: Vec<String>,
   },
   /// write exports.json from the spec on stdin.
   WriteExports {
      output: PathBuf,
   },
   /// drop docs and junk, unpack man pages, print the inventory as JSON.
   Prune {
      output: PathBuf,
   },
   /// make .pc and .cmake files, foo-config scripts and links relative to the
   /// store path.
   Relativize {
      prefix: String,
      dest:   String,
   },
   /// move the DWARF of what mummer linked into the debug output, by build-id.
   SplitDebug {
      output: PathBuf,
      debug:  PathBuf,
   },
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
   /// configure-time probe results shared through mummerd.
   Probe {
      #[pound(subcommand)]
      operation: ProbeOperation,
   },
   /// one of the build systems, make to bundler.
   System {
      #[pound(subcommand)]
      operation: SystemOperation,
   },
   Msbuild {
      file: PathBuf,
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
   /// bin/<name> as a launch record for a program with fixed arguments, --env
   /// a JSON object of variables set for it.
   Launcher {
      name:    String,
      program: String,
      args:    Vec<String>,
      #[pound(long, parse = serde_json::from_str::<BTreeMap<String, String>>, default = "{}")]
      env:     BTreeMap<String, String>,
   },
   /// the package on the structured attrs as one self-contained tree, for
   /// the bundle derivation.
   Bundle,
   /// the lock side of act.toml's bindings, what act.lock's sys should say.
   Syslibs {
      #[pound(subcommand)]
      operation: SyslibsOperation,
   },
}

#[derive(Parse)]
pub enum SyslibsOperation {
   /// our packages the lock files under `dir` can link for a build `with`
   /// these systems, as JSON. --tables names a JSON of them outside a
   /// package build.
   Wanted {
      dir:    PathBuf,
      with:   Vec<String>,
      #[pound(long)]
      tables: Option<PathBuf>,
   },
   /// the same for one ecosystem's locked names.
   WantedFor {
      ecosystem: Ecosystem,
      names:     Vec<String>,
      #[pound(long)]
      tables:    Option<PathBuf>,
   },
}

#[derive(Parse)]
pub enum DevOperation {
   Export,
   Prepare,
   Phase { name: String },
}

#[derive(Parse)]
pub enum ProbeOperation {
   /// the key for a kind of probe over its scripts and this build's inputs.
   Key {
      kind:    String,
      scripts: Vec<String>,
      #[pound(long, parse = serde_json::from_str::<Value>)]
      extra:   Option<Value>,
   },
   /// fetch a stored file, exit 1 when there is none.
   Restore {
      #[pound(parse = str::parse::<Key>)]
      key:  Key,
      file: PathBuf,
   },
   /// store a file for the next build with the same key.
   Store {
      #[pound(parse = str::parse::<Key>)]
      key:  Key,
      file: PathBuf,
   },
   /// fetch a stored directory tree, exit 1 when there is none.
   RestoreDir {
      #[pound(parse = str::parse::<Key>)]
      key: Key,
      dir: PathBuf,
   },
   /// store a directory tree.
   StoreDir {
      #[pound(parse = str::parse::<Key>)]
      key: Key,
      dir: PathBuf,
   },
   /// a CMakeCache.txt's probe results as a -C init script.
   CmakeInit { cache: PathBuf },
   /// restore the caches of the configures a build tool runs itself, and
   /// print the `CONFIG_SITE` that points each at its own.
   ConfiguresRestore {
      #[pound(parse = str::parse::<Key>)]
      key: Key,
   },
   /// store those of them whose answers replay.
   ConfiguresStore {
      #[pound(parse = str::parse::<Key>)]
      key: Key,
   },
}

#[derive(Parse)]
pub enum SystemOperation {
   SysEnv {
      ecosystem: Ecosystem,
   },
   /// the environment for the phases, as JSON, after creating the working
   /// directory.
   Setup {
      system: String,
   },
   /// the working directory.
   Workdir {
      system: String,
   },
   /// one phase, in the working directory.
   Phase {
      system: String,
      verb:   String,
   },
   /// configure settings that point a GHC bindist at our toolchain, one per
   /// line.
   GhcToolchain,
   /// install the unpacked GHC bindist in the working directory, or with
   /// --deb Debian's unpacked package.
   GhcInstall {
      #[pound(long)]
      deb: bool,
   },
}
