use std::{
   collections::BTreeMap,
   env::{
      current_exe,
      var,
   },
   ffi::OsString,
   fs::{
      File,
      copy,
      create_dir_all,
      read,
      read_dir,
      read_to_string,
      remove_dir_all,
      remove_file,
      write,
   },
   io::{
      self,
      Read as _,
   },
   os::unix::{
      ffi::OsStrExt as _,
      fs::symlink,
   },
   path::{
      Path,
      PathBuf,
   },
   process::Command,
   sync::OnceLock,
   time::{
      Duration,
      SystemTime,
   },
};

use mummer::{
   files::{
      sorted_entries,
      utf8,
      write_file,
   },
   process::strict,
   store::mask_hashes,
   store_path::StorePath,
   tree::{
      EntryKind,
      note,
      parallel,
      walk,
   },
};
use serde::Deserialize;
use tightrope::{
   client::Client,
   key::Key,
};

use crate::{
   attrs::{
      Attrs,
      Ctx,
      Kind,
      build_top,
   },
   prepare::succeed,
   probe,
   schema::{
      Condition,
      Field,
      PackagePath,
      Unsupported,
   },
   system::{
      command::{
         Env,
         capture,
         run,
         strs,
         tool,
         unknown,
      },
      dispatch::System,
   },
   test_cache,
   trace::Tests,
};

options! {
   Options {
      root ("root"): String = ".".to_owned()
         => "directory below the source the project lives in (monorepos, build/cmake)",
      deps ("deps"): PackagePath
         => "hackage repository to solve against, by default the shared set from locks/hackage.toml",
      flags ("flags"): Vec<String> = Vec::new()
         => "extra arguments for every cabal subcommand (--flags=\u{2026}, --allow-newer)",
      exes ("exes"): Vec<String> = Vec::new()
         => "exe components to build and install",
      project ("project"): String = String::new()
         => "extra cabal.project text (allow-newer:, constraints:)",
      project_file ("projectFile"): String = String::new()
         => "project file in place of cabal.project, as in cabal.release.project",
      suite_runs ("testOptions"): BTreeMap<String, Vec<Vec<String>>> = BTreeMap::new()
         => "per test-suite target, the argument lists of its runs, each its own cabal test",
   }
}

#[derive(Deserialize)]
struct Plan {
   #[serde(rename = "install-plan")]
   install_plan: Vec<Unit>,
}

#[derive(Deserialize)]
struct Unit {
   #[serde(rename = "type")]
   kind:      UnitType,
   #[serde(default)]
   style:     Option<UnitStyle>,
   id:        String,
   #[serde(rename = "pkg-name")]
   package:   String,
   #[serde(default, rename = "component-name")]
   component: Option<String>,
   #[serde(default, rename = "bin-file")]
   bin_file:  Option<PathBuf>,
}

#[derive(Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
enum UnitType {
   PreExisting,
   Configured,
   Installed,
}

#[derive(Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum UnitStyle {
   Global,
   Local,
   Inplace,
}

/// the same fixed path in every sandbox, so the paths inside cached units
/// agree.
const STORE: &str = "/build/cabal-store";

/// Where `test-prepare` records each test-suite executable's target.
const SUITES: &str = ".bigtop-cabal-tests.json";

pub struct Cabal;

impl System for Cabal {
   fn module(&self) -> &'static str {
      module_path!()
   }

   fn name(&self) -> &'static str {
      "cabal"
   }

   fn options(&self) -> io::Result<Vec<Field>> {
      Options::schema(self.name())
   }

   fn tests(&self) -> Option<&'static dyn Tests> {
      Some(&Self)
   }

   fn unsupported(&self) -> Option<Unsupported> {
      Some(Unsupported::new(
         Condition::Cross,
         "ghc-bootstrap only targets the build machine",
      ))
   }

   /// cabal writes its index cache into a noindex repository's directory, so
   /// the repository is a writable directory of symlinks. cabal passes -jsem
   /// to ghc itself, outside the ghc-options that unit ids hash, and jape
   /// feeds it from mummerd's slots.
   fn setup(&self, ctx: &Ctx, _attrs: &Attrs, _socket: &Path) -> io::Result<Env> {
      let options = ctx.options::<Options>("cabal")?;
      let repo = ctx.build.join("repo");
      create_dir_all(&repo)?;
      for source in sorted_entries(Path::new(&*options.deps))? {
         let Some(file_name) = source.file_name() else {
            continue;
         };
         let name = utf8(Path::new(file_name))?;
         if name.ends_with(".tar.gz") || Self::is_cabal(name) {
            symlink(format!("{}/{name}", options.deps), repo.join(name))?;
         }
      }
      let output = Command::new("ghc").arg("--info").output()?;
      if !output.status.success() {
         return Err(io::Error::other("ghc --info failed"));
      }
      let info = strict(&output.stdout, "ghc --info")?;
      let unit_id = info
         .split("\"Project Unit Id\",\"")
         .nth(1)
         .and_then(|rest| rest.split('"').next())
         .ok_or_else(|| io::Error::other("ghc --info names no Project Unit Id"))?;
      let cabal_dir = ctx.build.join("cabal");
      let units = format!("{STORE}/{unit_id}");
      let ghc_path = tool("ghc")?;
      let ghc_store = ghc_path
         .parent()
         .and_then(Path::parent)
         .and_then(|root| root.file_name())
         .ok_or_else(|| io::Error::other("ghc has no store basename"))
         .and_then(|name| Ok(utf8(Path::new(name))?.to_owned()))?;
      let unit_key = format!("hs/{ghc_store}");
      create_dir_all(&cabal_dir)?;
      create_dir_all(format!("{units}/package.db"))?;
      Self::recache(&units)?;
      let ghc_pkg = ctx.build.join("ghc-pkg/ghc-pkg");
      create_dir_all(ctx.build.join("ghc-pkg"))?;
      symlink(current_exe()?, &ghc_pkg)?;
      let ghc = ctx.build.join("ghc");
      write_file(
         &ghc,
         format!(
            "#!{}\nexec {} {} \"$@\"\n",
            tool("sh")?.display(),
            tool("jape")?.display(),
            tool("ghc")?.display()
         )
         .as_bytes(),
         Some(0o755),
      )?;
      write_file(
         &cabal_dir.join("config"),
         format!(
            "repository local\n  url: file+noindex://{}\nstore-dir: {STORE}\njobs: {}\nsemaphore: \
             True\nwith-compiler: {}\nwith-hc-pkg: {}\n",
            repo.display(),
            ctx.njobs,
            ghc.display(),
            ghc_pkg.display()
         )
         .as_bytes(),
         None,
      )?;
      let debug = if ctx.spec()?.debug {
         "  ghc-options: -g\n"
      } else {
         ""
      };
      write_file(
         Path::new(&format!("{}.local", Self::project_file(&options))),
         format!(
            "program-locations\n  gcc-location: {}\npackage *\n  split-sections: True\n{debug}{}",
            tool("cc")?.display(),
            options.project
         )
         .as_bytes(),
         None,
      )?;
      Ok([
         ("CABAL_DIR", utf8(&cabal_dir)?.to_owned()),
         ("CABAL_UNITS", units),
         ("CABAL_UNIT_KEY", unit_key),
         ("CABAL_GHC_PKG", utf8(&ghc_pkg)?.to_owned()),
      ]
      .into())
   }

   fn phase(&self, ctx: &Ctx, attrs: &Attrs, verb: &str, socket: &Path) -> io::Result<()> {
      let options = ctx.options::<Options>("cabal")?;
      let testing = Self::testing(ctx, attrs);
      match verb {
         "build" => Self::build(ctx, &options, testing, socket),
         "test-prepare" => Self::prepare_tests(ctx, &options),
         "test" => Self::test(ctx, &options),
         "install" => Self::install(ctx, &options, testing),
         _ => Err(unknown("cabal", verb)),
      }
   }
}

impl Cabal {
   fn is_cabal(name: &str) -> bool {
      Path::new(name)
         .extension()
         .is_some_and(|extension| extension.eq_ignore_ascii_case("cabal"))
   }

   fn exported(name: &str) -> io::Result<String> {
      var(name).map_err(|_unset| io::Error::other(format!("{name} is not set")))
   }

   fn recache(units: &str) -> io::Result<()> {
      succeed(
         Command::new("ghc-pkg")
            .arg("recache")
            .arg(format!("--package-db={units}/package.db")),
      )
   }

   fn testing(ctx: &Ctx, attrs: &Attrs) -> bool {
      ctx.tests_run
         && attrs.plan.phases.iter().any(|step| {
            matches!(step.kind, Kind::System { ref system, ref verb } if system == "cabal" && verb == "test")
         })
   }

   fn project_file(options: &Options) -> &str {
      if options.project_file.is_empty() {
         "cabal.project"
      } else {
         &options.project_file
      }
   }

   /// flags go to every cabal subcommand, build, test and list-bin must agree
   /// on one plan, so tests are enabled in all of them when the suite runs.
   fn flags(options: &Options, testing: bool) -> Vec<String> {
      let mut flags = options.flags.clone();
      if !options.project_file.is_empty() {
         flags.push(format!("--project-file={}", options.project_file));
      }
      if testing {
         flags.push("--enable-tests".to_owned());
      }
      flags
   }

   fn targets(options: &Options, testing: bool) -> Vec<String> {
      let mut args = Self::flags(options, testing);
      args.extend(options.exes.iter().map(|exe| format!("exe:{exe}")));
      args
   }

   fn cabal_files(ctx: &Ctx, options: &Options) -> io::Result<Vec<String>> {
      Ok(walk(&ctx.project_dir(&options.root))?
         .into_iter()
         .filter(|entry| {
            entry.kind == EntryKind::File
               && Self::is_cabal(&entry.rel)
               && !entry.rel.starts_with("dist-newstyle/")
         })
         .map(|entry| entry.path)
         .collect())
   }

   /// The package's own build tree, which ghc's recompilation check reuses
   /// module by module, where the units cache only covers dependencies. It
   /// is keyed on the package's identity alone, not its sources or inputs,
   /// since ghc hashes every module against the tree it finds.
   fn dist_key(ctx: &Ctx, options: &Options, testing: bool) -> io::Result<Key> {
      let project = ctx.project_dir(&options.root);
      let mut cabals = Self::cabal_files(ctx, options)?
         .into_iter()
         .map(|path| {
            match Path::new(&path).strip_prefix(&project) {
               Ok(relative) => Ok(utf8(relative)?.to_owned()),
               Err(_outside) => Ok(path),
            }
         })
         .collect::<io::Result<Vec<_>>>()?;
      cabals.sort();
      let record = serde_json::to_vec(&(
         cabals,
         Self::targets(options, testing),
         &options.project,
         &ctx.platform,
      ))?;
      let masked = mask_hashes(str::from_utf8(&record).map_err(io::Error::other)?);
      format!(
         "probe/cabal-dist/{}",
         blake3::hash(masked.as_bytes()).to_hex()
      )
      .parse()
   }

   fn plan(style: UnitStyle) -> io::Result<Vec<Unit>> {
      let plan = serde_json::from_str::<Plan>(&read_to_string("dist-newstyle/cache/plan.json")?)?;
      Ok(plan
         .install_plan
         .into_iter()
         .filter(|unit| unit.kind == UnitType::Configured && unit.style == Some(style))
         .collect())
   }

   fn unit_dirs(units: &str) -> io::Result<Vec<String>> {
      let mut names = Vec::new();
      for path in sorted_entries(Path::new(units))? {
         let name = utf8(Path::new(path.file_name().unwrap_or_default()))?.to_owned();
         if path.symlink_metadata()?.is_dir() && name != "package.db" && name != "incoming" {
            names.push(name);
         }
      }
      Ok(names)
   }

   fn restore(ctx: &Ctx, ids: &[String], socket: &Path) -> io::Result<()> {
      let units = Self::exported("CABAL_UNITS")?;
      let key = Self::exported("CABAL_UNIT_KEY")?;
      let got = parallel(ids, |id| {
         let unit = format!("{key}/{id}").parse::<Key>()?;
         let Some(bytes) = Client::connect(socket)
            .ok()
            .and_then(|mut client| client.get(&unit).ok().flatten())
         else {
            return Ok(false);
         };
         let tar = format!("{}/{id}.tar.zst", ctx.build.display());
         write_file(Path::new(&tar), &bytes, None)?;
         succeed(Command::new("bsdtar").args(["-xf", &tar, "-C", &units]))?;
         remove_file(&tar)?;
         Self::current_unit(ctx, &units, id)
      })?;
      Self::recache(&units)?;
      note(
         "cabal",
         &format!(
            "{}/{} units cached",
            got.iter().filter(|hit| **hit).count(),
            ids.len()
         ),
      );
      Ok(())
   }

   /// A unit's id hashes its C dependencies by name and version only, so a
   /// restored unit whose conf names another build of a dependency this one
   /// has, at the same version, is dropped for Cabal to build again.
   fn current_unit(ctx: &Ctx, units: &str, id: &str) -> io::Result<bool> {
      let conf = Path::new(units).join(format!("package.db/{id}.conf"));
      let text = match read_to_string(&conf) {
         Ok(text) => text,
         Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(true),
         Err(error) => return Err(error),
      };
      let current = ctx
         .roots
         .iter()
         .filter_map(|root| StorePath::containing(root))
         .collect::<Vec<StorePath>>();
      let stale = text
         .split(|character: char| character.is_whitespace() || matches!(character, '"' | ','))
         .filter_map(StorePath::containing)
         .any(|used| {
            !current.contains(&used) && current.iter().any(|root| root.name() == used.name())
         });
      if stale {
         remove_dir_all(Path::new(units).join(id))?;
         remove_file(&conf)?;
      }
      Ok(!stale)
   }

   /// executable units (alex, happy, build tools of other packages) have no
   /// package.db entry, just bin/.
   fn save_units(ctx: &Ctx, before: &[String], socket: &Path) -> io::Result<()> {
      let units = Self::exported("CABAL_UNITS")?;
      let key = Self::exported("CABAL_UNIT_KEY")?;
      let new = Self::unit_dirs(&units)?
         .into_iter()
         .filter(|id| !before.contains(id))
         .collect::<Vec<String>>();
      parallel(&new, |id| {
         let tar = format!("{}/{id}.tar.zst", ctx.build.display());
         let mut command = Command::new("bsdtar");
         command.args(["-c", "--zstd", "-f", &tar, "-C", &units, id]);
         let conf = format!("package.db/{id}.conf");
         if Path::new(&units).join(&conf).exists() {
            command.arg(&conf);
         }
         succeed(&mut command)?;
         if let Ok(mut client) = Client::connect(socket) {
            drop(client.put(&format!("{key}/{id}").parse()?, &read(&tar)?));
         }
         remove_file(&tar)
      })?;
      note("cabal", &format!("{} units stored", new.len()));
      Ok(())
   }

   /// Gives a restored tree one mtime, since ghc counts an object older than
   /// its interface as missing, and the sources a later one, since Cabal
   /// redoes C sources and alex or happy output only by mtime. The plan is
   /// dropped so Cabal configures against this build's inputs, and linked
   /// outputs so ghc links again, since it checks dependency libraries by
   /// mtime alone. Each component's `cache/build` record goes too, or Cabal
   /// hashes the touched sources, finds them unchanged and never relinks.
   fn settle_restored(project: &Path) -> io::Result<()> {
      let now = SystemTime::now();
      let restored = now
         .checked_sub(Duration::from_secs(1))
         .ok_or_else(|| io::Error::other("system clock before the epoch"))?;
      for entry in walk(project)? {
         if entry.kind != EntryKind::File {
            continue;
         }
         let built = entry.rel.starts_with("dist-newstyle/");
         if built && (entry.rel.ends_with("/cache/build") || Self::linked(Path::new(&entry.path))?)
         {
            remove_file(&entry.path)?;
            continue;
         }
         File::open(&entry.path)?.set_modified(if built { restored } else { now })?;
      }
      match remove_dir_all(project.join("dist-newstyle/cache")) {
         Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
         Ok(()) | Err(_) => Ok(()),
      }
   }

   /// A file a linker or archiver wrote, an executable, shared library or
   /// static archive, as opposed to an object ghc reuses.
   fn linked(path: &Path) -> io::Result<bool> {
      let mut header = [0_u8; 20];
      match File::open(path)?.read_exact(&mut header) {
         Ok(()) => {},
         Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(false),
         Err(error) => return Err(error),
      }
      let (&[magic, ident, _, macho_kind, elf_type], _) = header.as_chunks::<4>() else {
         return Ok(false);
      };
      let [_, order, _, _] = ident;
      let [first, second, _, _] = elf_type;
      let elf_kind = if order == 2 {
         u16::from_be_bytes([first, second])
      } else {
         u16::from_le_bytes([first, second])
      };
      Ok(match magic {
         [b'!', b'<', b'a', b'r'] | [b'M', b'Z', _, _] => true,
         [0x7F, b'E', b'L', b'F'] => matches!(elf_kind, 2 | 3),
         [0xCE | 0xCF, 0xFA, 0xED, 0xFE] => matches!(u32::from_le_bytes(macho_kind), 2 | 6),
         _ => false,
      })
   }

   fn build(ctx: &Ctx, options: &Options, testing: bool, socket: &Path) -> io::Result<()> {
      let targets = Self::targets(options, testing);
      let dist = if ctx.cache {
         let key = Self::dist_key(ctx, options, testing)?;
         let restored = probe::restore_dir(&key, Path::new("dist-newstyle"), socket)?;
         note("cabal-dist", if restored { "restored" } else { "cold" });
         if restored {
            Self::settle_restored(&ctx.project_dir(&options.root))?;
         }
         Some(key)
      } else {
         None
      };
      let mut dry = vec!["build", "--dry-run"];
      dry.extend(strs(&targets));
      run("cabal", &dry, &[])?;
      let planned = Self::plan(UnitStyle::Global)?;
      if ctx.cache {
         let ids = planned
            .into_iter()
            .map(|unit| unit.id)
            .collect::<Vec<String>>();
         Self::restore(ctx, &ids, socket)?;
      }
      let before = Self::unit_dirs(&Self::exported("CABAL_UNITS")?)?;
      let mut args = vec!["build"];
      args.extend(strs(&targets));
      run("cabal", &args, &[])?;
      if ctx.cache {
         Self::save_units(ctx, &before, socket)?;
      }
      if let Some(ref key) = dist {
         probe::store_dir(key, Path::new("dist-newstyle"), socket)?;
      }
      Ok(())
   }

   /// cabal test errors out (Cabal-7043) when the package declares no
   /// test-suite, which executable-only packages often do not.
   fn declares_tests(ctx: &Ctx, options: &Options) -> io::Result<bool> {
      let cabals = Self::cabal_files(ctx, options)?;
      let declares = |path: &String| {
         read_to_string(path).is_ok_and(|text| {
            text.lines().any(|line| {
               let lowered = line.trim_start().to_ascii_lowercase();
               lowered
                  .strip_prefix("test-suite")
                  .is_some_and(|rest| rest.starts_with(char::is_whitespace))
            })
         })
      };
      Ok(cabals.is_empty() || cabals.iter().any(declares))
   }

   /// Builds every test-suite outside the traced run and records which
   /// target each executable is, so the test cache keeps one unit per suite.
   fn prepare_tests(ctx: &Ctx, options: &Options) -> io::Result<()> {
      let mut suites = BTreeMap::new();
      if Self::declares_tests(ctx, options)? {
         let flags = Self::flags(options, true);
         let mut args = vec!["build"];
         args.extend(strs(&flags));
         args.push("all:tests");
         run("cabal", &args, &[])?;
         for unit in Self::plan(UnitStyle::Local)? {
            if let (Some(component), Some(bin)) = (unit.component, unit.bin_file)
               && component.starts_with("test:")
            {
               suites.insert(
                  bin.components().collect::<PathBuf>(),
                  format!("{}:{component}", unit.package),
               );
            }
         }
      }
      write(
         Path::new(&build_top()?).join(SUITES),
         serde_json::to_vec(&suites)?,
      )
   }

   fn test(ctx: &Ctx, options: &Options) -> io::Result<()> {
      if !Self::declares_tests(ctx, options)? {
         note("cabal", "no test suites");
         return Ok(());
      }
      let flags = Self::flags(options, true);
      let names = if let Some(chosen) = test_cache::selected::<String>()? {
         chosen
      } else {
         if !Path::new(&build_top()?).join(SUITES).exists() {
            Self::prepare_tests(ctx, options)?;
         }
         let all = suites()
            .ok_or_else(|| io::Error::other(format!("test-prepare wrote no {SUITES}")))?
            .values()
            .cloned()
            .collect::<Vec<String>>();
         if let Some(stray) = options.suite_runs.keys().find(|name| !all.contains(name)) {
            return Err(io::Error::other(format!(
               "cabal testOptions names {stray}, which is no test suite"
            )));
         }
         all
      };
      let (custom, plain) = names
         .iter()
         .partition::<Vec<&String>, _>(|name| options.suite_runs.contains_key(*name));
      if !plain.is_empty() {
         let mut args = vec!["test"];
         args.extend(strs(&flags));
         args.extend(plain.iter().map(|name| name.as_str()));
         run("cabal", &args, &[])?;
      }
      for name in custom {
         for arguments in &options.suite_runs[name] {
            let passed = arguments
               .iter()
               .map(|arg| format!("--test-option={arg}"))
               .collect::<Vec<String>>();
            let mut args = vec!["test"];
            args.extend(strs(&flags));
            args.push(name);
            args.extend(strs(&passed));
            run("cabal", &args, &[])?;
         }
      }
      Ok(())
   }

   fn install(ctx: &Ctx, options: &Options, testing: bool) -> io::Result<()> {
      let bin = ctx.out.join("bin");
      create_dir_all(&bin)?;
      let flags = Self::flags(options, testing);
      for exe in &options.exes {
         let target = format!("exe:{exe}");
         let mut args = vec!["list-bin"];
         args.extend(strs(&flags));
         args.push(&target);
         let built = capture("cabal", &args)?;
         copy(built.trim(), bin.join(exe))?;
      }
      Ok(())
   }
}

fn suites() -> Option<&'static BTreeMap<PathBuf, String>> {
   static SUITES_READ: OnceLock<Option<BTreeMap<PathBuf, String>>> = OnceLock::new();
   SUITES_READ
      .get_or_init(|| {
         let bytes = read(Path::new(&build_top().ok()?).join(SUITES)).ok()?;
         serde_json::from_slice(&bytes).ok()
      })
      .as_ref()
}

/// A test-suite is its executable and what that starts.
impl Tests for Cabal {
   /// cabal execs a suite as `<project>/./dist-newstyle/…`, plan.json spells
   /// it without the `.`.
   fn unit(&self, exe: &Path, _args: &[OsString], _cwd: &Path) -> Option<String> {
      suites()?
         .get(&exe.components().collect::<PathBuf>())
         .cloned()
   }

   fn prepares(&self) -> bool {
      true
   }

   /// cabal rewrites its plan cache and each suite's log on every run.
   fn state(&self, path: &Path) -> bool {
      path.ancestors().any(|directory| {
         directory.ends_with("dist-newstyle/cache") || directory.ends_with("dist-newstyle/test")
      })
   }
}

const DATABASE_CHANGES: [&str; 8] = [
   "register",
   "update",
   "unregister",
   "recache",
   "expose",
   "hide",
   "trust",
   "distrust",
];

pub fn ordered_ghc_pkg(args: &[String]) -> io::Result<i32> {
   let own = current_exe()?;
   let real = var("PATH")
      .unwrap_or_default()
      .split(':')
      .map(|directory| Path::new(directory).join("ghc-pkg"))
      .find(|candidate| candidate.canonicalize().is_ok_and(|found| found != own))
      .ok_or_else(|| io::Error::other("no ghc-pkg on PATH besides bigtop"))?;
   let changes = args
      .iter()
      .any(|arg| DATABASE_CHANGES.contains(&arg.as_str()));
   let mut databases = package_dbs(args);
   databases.sort();
   let top = var("NIX_BUILD_TOP").map_err(|_unset| io::Error::other("NIX_BUILD_TOP is not set"))?;
   let locks = databases
      .iter()
      .map(|db| {
         let canonical = Path::new(db).canonicalize()?;
         let lock = File::create(format!(
            "{top}/.ghc-pkg-{}.lock",
            blake3::hash(canonical.as_os_str().as_bytes())
         ))?;
         if changes {
            lock.lock()?;
         } else {
            lock.lock_shared()?;
         }
         Ok(lock)
      })
      .collect::<io::Result<Vec<File>>>()?;
   if !changes {
      let status = Command::new(&real).args(args).status()?;
      drop(locks);
      return Ok(status.code().unwrap_or(1));
   }
   for db in &databases {
      in_name_order(Path::new(db))?;
   }
   let status = Command::new(&real).args(args).status()?;
   if status.success() {
      for db in &databases {
         in_name_order(Path::new(db))?;
         succeed(
            Command::new(&real)
               .arg("recache")
               .arg(format!("--package-db={db}")),
         )?;
      }
   }
   drop(locks);
   Ok(status.code().unwrap_or(1))
}

fn package_dbs(args: &[String]) -> Vec<String> {
   let mut databases = Vec::new();
   let mut rest = args.iter();
   while let Some(arg) = rest.next() {
      let given = if matches!(arg.as_str(), "--package-db" | "--package-conf" | "-f") {
         rest.next().cloned()
      } else {
         ["--package-db=", "--package-conf=", "-f"]
            .iter()
            .find_map(|flag| arg.strip_prefix(flag))
            .map(str::to_owned)
      };
      if let Some(db) = given
         && Path::new(&db).is_dir()
         && !databases.contains(&db)
      {
         databases.push(db);
      }
   }
   databases
}

pub fn in_name_order(db: &Path) -> io::Result<()> {
   let mut confs = read_dir(db)?
      .map(|entry| Ok(entry?.path()))
      .collect::<io::Result<Vec<_>>>()?;
   confs.retain(|path| {
      path
         .extension()
         .is_some_and(|extension| extension == "conf")
   });
   confs.sort();
   let saved = confs
      .into_iter()
      .map(|path| {
         let bytes = read(&path)?;
         Ok((path, bytes))
      })
      .collect::<io::Result<Vec<_>>>()?;
   for conf in &saved {
      remove_file(&conf.0)?;
   }
   for conf in &saved {
      write(&conf.0, &conf.1)?;
   }
   Ok(())
}
