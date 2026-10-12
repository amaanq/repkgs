use std::{
   collections::BTreeMap,
   ffi::OsString,
   fs::read,
   io,
   path::{
      Path,
      PathBuf,
   },
   process::{
      Command,
      Stdio,
   },
   sync::OnceLock,
};

use mummer::{
   files::{
      utf8,
      write_file,
   },
   store::{
      mask_hashes,
      on_path,
   },
   tree::{
      EntryKind,
      note,
      walk,
   },
};
use serde::{
   Deserialize,
   Serialize,
   de::IgnoredAny,
};

use crate::{
   attrs::{
      Attrs,
      Ctx,
      Def,
      build_top,
   },
   probe,
   schema::Field,
   system::{
      command::{
         run,
         strs,
         unknown,
         upsert,
      },
      dispatch::System,
   },
   test_cache,
   trace::{
      Commands,
      TestNames,
      Tests,
   },
};

options! {
   Options {
      root ("root"): String = ".".to_owned()
         => "directory below the source the project lives in (monorepos, build/cmake)",
      defs ("defs"): BTreeMap<String, Def> = BTreeMap::new()
         => "-D cache entries. true/false render ON/OFF, packages their store path",
      generator ("generator"): String = "Ninja".to_owned()
         => "cmake -G",
      flags ("flags"): Vec<String> = Vec::new()
         => "extra arguments for cmake at configure time",
      skip_tests ("skipTests"): Vec<String> = Vec::new()
         => "ctest -E regexes",
   }
}

#[derive(Serialize)]
struct Extra<'options> {
   defs:      BTreeMap<&'options str, Def>,
   generator: &'options str,
   flags:     &'options [String],
}

pub struct Cmake;

impl System for Cmake {
   fn module(&self) -> &'static str {
      module_path!()
   }

   fn name(&self) -> &'static str {
      "cmake"
   }

   fn options(&self) -> io::Result<Vec<Field>> {
      Options::schema(self.name())
   }

   fn phases(&self) -> &'static [&'static str] {
      &["configure", "build", "test", "install"]
   }

   fn workdir(&self, ctx: &Ctx) -> io::Result<PathBuf> {
      Ok(ctx.build.clone())
   }

   fn tests(&self) -> Option<&'static dyn Tests> {
      Some(&Self)
   }

   fn phase(&self, ctx: &Ctx, attrs: &Attrs, verb: &str, socket: &Path) -> io::Result<()> {
      let options = ctx.options::<Options>(self.name())?;
      match verb {
         "configure" => Self::configure(ctx, attrs, &options, socket),
         "build" => {
            let jobs = format!("-j{}", ctx.njobs);
            run("cmake", &["--build", ".", &jobs], &[])
         },
         "test-prepare" => Self::list_tests(&options),
         "test" => {
            let jobs = ctx.test_jobs()?.to_string();
            let skip = options.skip_tests.join("|");
            let mut args = vec!["--output-on-failure", "-j", &jobs];
            if !options.skip_tests.is_empty() {
               args.extend(["-E", &skip]);
            }
            let Some(selected) = test_cache::selected::<TestNames>()? else {
               return run("ctest", &args, &[]);
            };
            let names = selected
               .into_iter()
               .flat_map(|test| test.names)
               .collect::<Vec<_>>()
               .join("\n");
            let list = Path::new(&build_top()?).join(SELECTED);
            write_file(&list, names.as_bytes(), None)?;
            let file = utf8(&list)?;
            args.extend(["--tests-from-file", file]);
            run("ctest", &args, &[])
         },
         "install" => run("cmake", &["--install", "."], &[]),
         _ => Err(unknown(self.name(), verb)),
      }
   }
}

impl Cmake {
   /// What ctest would run, written where the test phase's tracer reads it.
   fn list_tests(options: &Options) -> io::Result<()> {
      let skip = options.skip_tests.join("|");
      let mut args = vec!["--show-only=json-v1"];
      if !options.skip_tests.is_empty() {
         args.extend(["-E", &skip]);
      }
      let output = Command::new("ctest")
         .args(&args)
         .stderr(Stdio::inherit())
         .output()?;
      if !output.status.success() {
         return Err(io::Error::other(format!(
            "ctest --show-only failed, {}",
            output.status
         )));
      }
      write_file(
         &Path::new(&build_top()?).join(LISTING),
         &output.stdout,
         None,
      )
   }

   fn render(def: &Def) -> String {
      match *def {
         Def::Flag(true) => "ON".to_owned(),
         Def::Flag(false) => "OFF".to_owned(),
         Def::Text(ref text) => text.clone(),
         Def::Number(number) => number.to_string(),
      }
   }

   /// Darwin.cmake finds usr/ and the frameworks from `CMAKE_OSX_SYSROOT`.
   fn defaults(ctx: &Ctx, attrs: &Attrs, options: &Options) -> io::Result<Vec<(String, Def)>> {
      let platform = ctx.platform()?;
      let tree = attrs
         .probe_inputs
         .first()
         .ok_or_else(|| io::Error::other("the builder tree is not among probeInputs"))?;

      let mut defs = vec![
         (
            "CMAKE_INSTALL_PREFIX".to_owned(),
            Def::Text(utf8(&ctx.out)?.to_owned()),
         ),
         (
            "CMAKE_BUILD_TYPE".to_owned(),
            Def::Text("Release".to_owned()),
         ),
         (
            "CMAKE_INSTALL_LIBDIR".to_owned(),
            Def::Text("lib".to_owned()),
         ),
         (
            "CMAKE_INSTALL_SBINDIR".to_owned(),
            Def::Text("bin".to_owned()),
         ),
         (
            "CMAKE_PREFIX_PATH".to_owned(),
            Def::Text(
               ctx.deps
                  .iter()
                  .map(|dep| dep.root.as_str())
                  .collect::<Vec<_>>()
                  .join(";"),
            ),
         ),
         (
            "CMAKE_SYSTEM_PREFIX_PATH".to_owned(),
            Def::Text(platform.sysroot.clone()),
         ),
         (
            "CMAKE_PROJECT_TOP_LEVEL_INCLUDES".to_owned(),
            Def::Text(format!(
               "{tree}/systems/sysroot-libs.cmake;{tree}/systems/custom-launch.cmake"
            )),
         ),
         (
            "PKGS_SYSROOT".to_owned(),
            Def::Text(platform.sysroot.clone()),
         ),
         ("BUILD_SHARED_LIBS".to_owned(), Def::Flag(true)),
         ("BUILD_TESTING".to_owned(), Def::Flag(ctx.tests_run)),
      ];
      let mummer = on_path("mummer");
      if mummer.is_absolute() {
         defs.push((
            "PKGS_CUSTOM_LAUNCHER".to_owned(),
            Def::Text(format!("{} tblgenwrap", utf8(&mummer)?)),
         ));
      }
      if platform.cross {
         upsert(
            &mut defs,
            "CMAKE_SYSTEM_NAME",
            Def::Text(platform.os_names.cmake),
         );
         upsert(&mut defs, "CMAKE_SYSTEM_PROCESSOR", Def::Text(platform.cpu));

         // a hand-set CMAKE_SYSTEM_NAME leaves the version empty, and projects
         // such as zeromq derive _WIN32_WINNT from it
         if platform.os == "windows" {
            upsert(
               &mut defs,
               "CMAKE_SYSTEM_VERSION",
               Def::Text("10.0".to_owned()),
            );
         }
      }
      if platform.os == "macos" {
         upsert(
            &mut defs,
            "CMAKE_OSX_SYSROOT",
            Def::Text(platform.sysroot.clone()),
         );
      }
      if !platform.emulator.is_empty() {
         upsert(
            &mut defs,
            "CMAKE_CROSSCOMPILING_EMULATOR",
            Def::Text(platform.emulator.join(";")),
         );
      }
      for (name, value) in &options.defs {
         upsert(&mut defs, name, value.clone());
      }
      Ok(defs)
   }

   fn configure(ctx: &Ctx, attrs: &Attrs, options: &Options, socket: &Path) -> io::Result<()> {
      let defs = Self::defaults(ctx, attrs, options)?;
      let srcdir = ctx.project_dir(&options.root);
      let scripts = walk(&srcdir)?
         .into_iter()
         .filter(|entry| entry.kind == EntryKind::File)
         .map(|entry| entry.path)
         .collect::<Vec<String>>();
      let key = probe::key(
         "cmake",
         &scripts,
         Some(&Extra {
            defs:      defs
               .iter()
               .map(|entry| {
                  (entry.0.as_str(), match entry.1 {
                     Def::Text(ref text) => Def::Text(mask_hashes(text)),
                     ref other @ (Def::Flag(_) | Def::Number(_)) => other.clone(),
                  })
               })
               .collect(),
            generator: &options.generator,
            flags:     &options.flags,
         }),
      )?;
      let init = format!("{}/probe-init.cmake", ctx.build.display());
      let restored = probe::restore(&key, Path::new(&init), socket)?;
      note("cmake-probes", if restored { "restored" } else { "cold" });
      let source = utf8(&srcdir)?;
      let rendered = defs
         .iter()
         .map(|entry| format!("-D{}={}", entry.0, Self::render(&entry.1)))
         .collect::<Vec<String>>();
      let mut args = vec!["-S", &source, "-B", ".", "-G", &options.generator];
      if restored {
         args.extend(["-C", &init]);
      }
      args.extend(strs(&rendered));
      args.extend(strs(&options.flags));
      run("cmake", &args, &[])?;
      if !restored {
         write_file(
            Path::new(&init),
            probe::cmake_init(Path::new("CMakeCache.txt"))?.as_bytes(),
            None,
         )?;
         probe::store(&key, Path::new(&init), socket)?;
      }
      Ok(())
   }
}

/// Where `test-prepare` leaves ctest's listing of its tests.
const LISTING: &str = ".bigtop-ctest-tests.json";
/// Where the test phase lists the tests ctest should limit itself to.
const SELECTED: &str = ".bigtop-ctest-selected";

#[derive(Deserialize)]
struct Listing {
   tests: Vec<Listed>,
}

#[derive(Deserialize)]
struct Listed {
   name:       String,
   /// Empty for a test whose executable is not built.
   #[serde(default)]
   command:    Vec<String>,
   #[serde(default)]
   properties: Vec<Property>,
}

#[derive(Deserialize)]
struct Property {
   name:  String,
   value: PropertyValue,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum PropertyValue {
   Text(String),
   Other(IgnoredAny),
}

fn commands() -> Option<&'static Commands> {
   static COMMANDS: OnceLock<Option<Commands>> = OnceLock::new();
   COMMANDS
      .get_or_init(|| {
         let bytes = read(Path::new(&build_top().ok()?).join(LISTING)).ok()?;
         let listing = serde_json::from_slice::<Listing>(&bytes).ok()?;
         let mut found = Commands::default();
         for test in listing.tests {
            let directory = test.properties.into_iter().find_map(|property| {
               match (property.name.as_str(), property.value) {
                  ("WORKING_DIRECTORY", PropertyValue::Text(path)) => Some(path),
                  _ => None,
               }
            });
            if let Some(cwd) = directory {
               found.add(test.name, test.command, cwd);
            }
         }
         Some(found)
      })
      .as_ref()
}

/// ctest execs a test's command as listed, so a test's process runs it, or
/// runs its interpreter when the command is a script.
impl Tests for Cmake {
   fn unit(&self, _exe: &Path, args: &[OsString], cwd: &Path) -> Option<String> {
      commands()?.unit(args, cwd)
   }

   fn prepares(&self) -> bool {
      true
   }

   /// The logs and cost data every ctest run rewrites.
   fn state(&self, path: &Path) -> bool {
      path
         .ancestors()
         .any(|directory| directory.ends_with("Testing/Temporary"))
   }
}
