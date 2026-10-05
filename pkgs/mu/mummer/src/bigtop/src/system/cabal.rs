use std::{
   env::var,
   fs::{
      File,
      copy,
      create_dir_all,
      read,
      read_dir,
      read_to_string,
      remove_file,
   },
   io,
   os::unix::fs::symlink,
   path::Path,
   process::Command,
   time::SystemTime,
};

use mummer::{
   files::write_file,
   tree::{
      note,
      parallel,
      walk,
   },
};
use serde::Deserialize;
use tightrope::client::Client;

use crate::{
   attrs::{
      Attrs,
      Ctx,
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
   kind:     String,
   #[serde(default)]
   style:    Option<String>,
   id:       String,
   #[serde(rename = "pkg-name")]
   pkg_name: String,
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
      let options: Options = ctx.options("cabal")?;
      let repo = format!("{}/repo", ctx.build);
      create_dir_all(&repo)?;
      let mut sources: Vec<String> = read_dir(&options.deps)?
         .map(|entry| entry.map(|found| found.file_name().to_string_lossy().into_owned()))
         .collect::<io::Result<_>>()?;
      sources.sort();
      for name in sources
         .iter()
         .filter(|name| name.ends_with(".tar.gz") || Self::is_cabal(name))
      {
         symlink(format!("{}/{name}", options.deps), format!("{repo}/{name}"))?;
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
      let cabal_dir = format!("{}/cabal", ctx.build);
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
      let ghc = format!("{}/ghc", ctx.build);
      write_file(
         Path::new(&ghc),
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
         Path::new(&format!("{cabal_dir}/config")),
         format!(
            "repository local\n  url: file+noindex://{repo}\nstore-dir: {STORE}\njobs: \
             {}\nsemaphore: True\nwith-compiler: {ghc}\nwith-hc-pkg: {}\n",
            ctx.njobs,
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
         ("CABAL_DIR", cabal_dir),
         ("CABAL_UNITS", units),
         ("CABAL_UNIT_KEY", unit_key),
      ]
      .into())
   }

   fn phase(&self, ctx: &Ctx, attrs: &Attrs, verb: &str, socket: &Path) -> io::Result<()> {
      let options: Options = ctx.options("cabal")?;
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

   fn units() -> io::Result<String> {
      var("CABAL_UNITS").map_err(|_unset| io::Error::other("CABAL_UNITS is not set"))
   }

   fn unit_key() -> io::Result<String> {
      var("CABAL_UNIT_KEY").map_err(|_unset| io::Error::other("CABAL_UNIT_KEY is not set"))
   }

   fn recache(units: &str) -> io::Result<()> {
      succeed(
         Command::new("ghc-pkg")
            .arg("recache")
            .arg(format!("--package-db={units}/package.db")),
         "ghc-pkg",
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
            entry.kind == 'f'
               && Self::is_cabal(&entry.rel)
               && !entry.rel.starts_with("dist-newstyle/")
         })
         .map(|entry| entry.path)
         .collect())
   }

   /// The package's own build tree, which ghc's recompilation check reuses
   /// module by module, where the units cache only covers dependencies.
   fn dist_key(ctx: &Ctx, options: &Options) -> io::Result<String> {
      probe::key(
         "cabal-dist",
         &Self::cabal_files(ctx, options)?,
         Some(&(Self::targets(options), &options.project)),
      )
   }

   fn plan() -> io::Result<Vec<Unit>> {
      let plan = serde_json::from_str::<Plan>(&read_to_string("dist-newstyle/cache/plan.json")?)?;
      Ok(plan
         .install_plan
         .into_iter()
         .filter(|unit| unit.kind == "configured" && unit.style.as_deref() == Some("global"))
         .collect())
   }

   fn unit_dirs(units: &str) -> io::Result<Vec<String>> {
      let mut names = Vec::new();
      for entry in read_dir(units)? {
         let found = entry?;
         let name = found.file_name().to_string_lossy().into_owned();
         if found.file_type()?.is_dir() && name != "package.db" && name != "incoming" {
            names.push(name);
         }
      }
      names.sort();
      Ok(names)
   }

   fn restore(ctx: &Ctx, ids: &[String], socket: &Path) -> io::Result<()> {
      let units = Self::units()?;
      let key = Self::unit_key()?;
      let got: Vec<bool> = parallel(ids, |id| {
         let Some(bytes) = Client::connect(socket)
            .ok()
            .and_then(|mut client| client.get(&format!("{key}/{id}")).ok().flatten())
         else {
            return Ok(false);
         };
         let tar = format!("{}/{id}.tar.zst", ctx.build);
         write_file(Path::new(&tar), &bytes, None)?;
         succeed(
            Command::new("bsdtar").args(["-xf", &tar, "-C", &units]),
            "bsdtar",
         )?;
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
      let units = Self::units()?;
      let key = Self::unit_key()?;
      let new: Vec<String> = Self::unit_dirs(&units)?
         .into_iter()
         .filter(|id| !before.contains(id))
         .collect();
      parallel(&new, |id| {
         let tar = format!("{}/{id}.tar.zst", ctx.build);
         let mut command = Command::new("bsdtar");
         command.args(["-c", "--zstd", "-f", &tar, "-C", &units, id]);
         let conf = format!("package.db/{id}.conf");
         if Path::new(&units).join(&conf).exists() {
            command.arg(&conf);
         }
         succeed(&mut command, "bsdtar")?;
         if let Ok(mut client) = Client::connect(socket) {
            drop(client.put(&format!("{key}/{id}"), &read(&tar)?));
         }
         remove_file(&tar)
      })?;
      note("cabal", &format!("{} units stored", new.len()));
      Ok(())
   }

   fn build(ctx: &Ctx, attrs: &Attrs, options: &Options, socket: &Path) -> io::Result<()> {
      let targets = Self::targets(options);
      let dist = if ctx.cache {
         let key = Self::dist_key(ctx, options)?;
         let restored = probe::restore_dir(&key, Path::new("dist-newstyle"), socket)?;
         note("cabal-dist", if restored { "restored" } else { "cold" });
         if restored {
            // ghc counts an object older than its interface as missing
            let now = SystemTime::now();
            for entry in walk(Path::new("dist-newstyle"))? {
               if entry.kind == 'f' {
                  File::open(&entry.path)?.set_modified(now)?;
               }
            }
         }
         Some(key)
      } else {
         None
      };
      let mut dry = vec!["build", "--dry-run"];
      dry.extend(strs(&targets));
      run("cabal", &dry, &[])?;
      let planned = Self::plan()?;
      let names: Vec<String> = planned.iter().map(|unit| unit.pkg_name.clone()).collect();
      syslibs::check_names(
         &syslibs::wanted_for(attrs, "hackage", &names)?,
         ctx.spec()?.sys.as_deref(),
         "lock",
      )?;
      if ctx.cache {
         let ids: Vec<String> = planned.into_iter().map(|unit| unit.id).collect();
         Self::restore(ctx, &ids, socket)?;
      }
      let before = Self::unit_dirs(&Self::units()?)?;
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
      let bin = format!("{}/bin", ctx.out);
      create_dir_all(&bin)?;
      for exe in &options.exes {
         let target = format!("exe:{exe}");
         let mut args = vec!["list-bin"];
         args.extend(strs(&options.flags));
         args.push(&target);
         let built = capture("cabal", &args)?;
         copy(built.trim(), format!("{bin}/{exe}"))?;
      }
      Ok(())
   }
}
