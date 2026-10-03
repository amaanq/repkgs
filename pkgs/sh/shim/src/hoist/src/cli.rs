use std::path::PathBuf;

use pound::Parse;

/// the build script executor for manicpkgs, everything between the structured
/// attrs and the finished output tree.
#[derive(Parse)]
#[pound(name = "hoist")]
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
   /// move the DWARF of what shim linked into the debug output, by build-id.
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
   /// configure-time probe results shared through shimd.
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
      #[pound(long)]
      env:     Option<String>,
   },
   /// the package on the structured attrs as one self-contained tree, for
   /// the bundle derivation.
   Bundle,
   /// the lock side of sys-libs.json, what [pin] sys should say.
   Syslibs {
      #[pound(subcommand)]
      operation: SyslibsOperation,
   },
}

#[derive(Parse)]
pub enum SyslibsOperation {
   /// our packages the lock files under `dir` can link, as JSON. --tables
   /// names sys-libs.json outside a package build.
   Wanted {
      dir:    PathBuf,
      #[pound(long)]
      tables: Option<PathBuf>,
   },
   /// the same for one ecosystem's locked names.
   WantedFor {
      ecosystem: String,
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
   SysEnv {
      ecosystem: String,
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
