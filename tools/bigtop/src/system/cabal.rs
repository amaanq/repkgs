use std::{
   env::var,
   fs::{
      File,
      copy,
      create_dir_all,
      read,
      read_to_string,
      remove_dir_all,
      remove_file,
   },
   io::{
      self,
      Read as _,
   },
   os::unix::fs::symlink,
   path::Path,
   process::Command,
   time::{
      Duration,
      SystemTime,
   },
};

use mummer::{
   files::{
      sorted_entries,
      write_file,
   },
   syslibs::Ecosystem,
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
      utf8,
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
      syslibs,
   },
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
   kind:     UnitType,
   #[serde(default)]
   style:    Option<UnitStyle>,
   id:       String,
   #[serde(rename = "pkg-name")]
   pkg_name: String,
}

#[derive(Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
enum UnitType {
   PreExisting,
   Configured,
   Installed,
}

#[derive(Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum UnitStyle {
   Global,
   Local,
   Inplace,
}

/// the same fixed path in every sandbox, so the paths inside cached units
/// agree.
const STORE: &str = "/build/cabal-store";

pub struct Cabal;

impl System for Cabal {
   fn name(&self) -> &'static str {
      "cabal"
   }

   fn options(&self) -> io::Result<Vec<Field>> {
      Options::schema(self.name())
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
         let Some(name) = source
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
         else {
            continue;
         };
         if name.ends_with(".tar.gz") || Self::is_cabal(&name) {
            symlink(format!("{}/{name}", options.deps), repo.join(&name))?;
         }
      }
      let output = Command::new("ghc").arg("--info").output()?;
      if !output.status.success() {
         return Err(io::Error::other("ghc --info failed"));
      }
      let info = String::from_utf8_lossy(&output.stdout);
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
         .map(|name| name.to_string_lossy().into_owned())
         .ok_or_else(|| io::Error::other("ghc has no store basename"))?;
      let unit_key = format!("hs/{ghc_store}");
      create_dir_all(&cabal_dir)?;
      create_dir_all(format!("{units}/package.db"))?;
      Self::recache(&units)?;
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
            tool("ghc-pkg")?.display()
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
         Path::new("cabal.project.local"),
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
      ]
      .into())
   }

   fn phase(&self, ctx: &Ctx, attrs: &Attrs, verb: &str, socket: &Path) -> io::Result<()> {
      let options = ctx.options::<Options>("cabal")?;
      match verb {
         "build" => Self::build(ctx, attrs, &options, socket),
         "test" => Self::test(ctx, &options),
         "install" => Self::install(ctx, &options),
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

   /// flags go to every cabal subcommand, build, test and list-bin must agree.
   fn targets(options: &Options) -> Vec<String> {
      let mut args = options.flags.clone();
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
   fn dist_key(ctx: &Ctx, options: &Options) -> io::Result<Key> {
      let project = ctx.project_dir(&options.root);
      let mut cabals = Self::cabal_files(ctx, options)?
         .into_iter()
         .map(|path| {
            match Path::new(&path).strip_prefix(&project) {
               Ok(relative) => relative.to_string_lossy().into_owned(),
               Err(_outside) => path,
            }
         })
         .collect::<Vec<_>>();
      cabals.sort();
      let record = serde_json::to_vec(&(
         cabals,
         Self::targets(options),
         &options.project,
         &ctx.platform,
      ))?;
      format!("probe/cabal-dist/{}", blake3::hash(&record).to_hex()).parse()
   }

   fn plan() -> io::Result<Vec<Unit>> {
      let plan = serde_json::from_str::<Plan>(&read_to_string("dist-newstyle/cache/plan.json")?)?;
      Ok(plan
         .install_plan
         .into_iter()
         .filter(|unit| unit.kind == UnitType::Configured && unit.style == Some(UnitStyle::Global))
         .collect())
   }

   fn unit_dirs(units: &str) -> io::Result<Vec<String>> {
      let mut names = Vec::new();
      for path in sorted_entries(Path::new(units))? {
         let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
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
         Ok(true)
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
   /// mtime alone.
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
         if built && Self::linked(Path::new(&entry.path))? {
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

   fn build(ctx: &Ctx, attrs: &Attrs, options: &Options, socket: &Path) -> io::Result<()> {
      let targets = Self::targets(options);
      let dist = if ctx.cache {
         let key = Self::dist_key(ctx, options)?;
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
      let planned = Self::plan()?;
      let names = planned
         .iter()
         .map(|unit| unit.pkg_name.clone())
         .collect::<Vec<String>>();
      syslibs::check_names(
         &syslibs::wanted_for(attrs, Ecosystem::Hackage, &names)?,
         ctx.spec()?.sys.as_deref(),
         "lock",
      )?;
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
   fn test(ctx: &Ctx, options: &Options) -> io::Result<()> {
      let cabals = Self::cabal_files(ctx, options)?;
      let declares_tests = |path: &String| {
         read_to_string(path).is_ok_and(|text| {
            text.lines().any(|line| {
               let lowered = line.trim_start().to_ascii_lowercase();
               lowered
                  .strip_prefix("test-suite")
                  .is_some_and(|rest| rest.starts_with(char::is_whitespace))
            })
         })
      };
      if !cabals.is_empty() && !cabals.iter().any(declares_tests) {
         note("cabal", "no test suites");
         return Ok(());
      }
      let mut args = vec!["test", "--enable-tests"];
      args.extend(strs(&options.flags));
      args.push("all:tests");
      run("cabal", &args, &[])
   }

   fn install(ctx: &Ctx, options: &Options) -> io::Result<()> {
      let bin = ctx.out.join("bin");
      create_dir_all(&bin)?;
      for exe in &options.exes {
         let target = format!("exe:{exe}");
         let mut args = vec!["list-bin"];
         args.extend(strs(&options.flags));
         args.push(&target);
         let built = capture("cabal", &args)?;
         copy(built.trim(), bin.join(exe))?;
      }
      Ok(())
   }
}
