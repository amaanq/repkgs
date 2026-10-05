use std::{
   collections::BTreeMap,
   io,
   path::{
      Path,
      PathBuf,
   },
};

use mummer::{
   files::write_file,
   tree::{
      EntryKind,
      note,
      walk,
   },
};
use serde::Serialize;

use crate::{
   attrs::{
      Attrs,
      Ctx,
      Def,
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
   defs:      BTreeMap<&'options str, &'options Def>,
   generator: &'options str,
   flags:     &'options [String],
}

pub struct Cmake;

impl System for Cmake {
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
      Ok(PathBuf::from(&ctx.build))
   }

   fn phase(&self, ctx: &Ctx, attrs: &Attrs, verb: &str, socket: &Path) -> io::Result<()> {
      let options = ctx.options::<Options>(self.name())?;
      match verb {
         "configure" => Self::configure(ctx, attrs, &options, socket),
         "build" => {
            let jobs = format!("-j{}", ctx.njobs);
            run("cmake", &["--build", ".", &jobs], &[])
         },
         "test" => {
            let jobs = ctx.test_jobs()?.to_string();
            let skip = options.skip_tests.join("|");
            let mut args = vec!["--output-on-failure", "-j", &jobs];
            if !options.skip_tests.is_empty() {
               args.extend(["-E", &skip]);
            }
            run("ctest", &args, &[])
         },
         "install" => run("cmake", &["--install", "."], &[]),
         _ => Err(unknown(self.name(), verb)),
      }
   }
}

impl Cmake {
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
            Def::Text(ctx.out.clone()),
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
            Def::Text(format!("{tree}/systems/sysroot-libs.cmake")),
         ),
         (
            "PKGS_SYSROOT".to_owned(),
            Def::Text(platform.sysroot.clone()),
         ),
         ("BUILD_SHARED_LIBS".to_owned(), Def::Flag(true)),
         ("BUILD_TESTING".to_owned(), Def::Flag(ctx.tests_run)),
      ];
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
               .map(|entry| (entry.0.as_str(), &entry.1))
               .collect(),
            generator: &options.generator,
            flags:     &options.flags,
         }),
      )?;
      let init = format!("{}/probe-init.cmake", ctx.build);
      let restored = probe::restore(&key, Path::new(&init), socket)?;
      note("cmake-probes", if restored { "restored" } else { "cold" });
      let source = srcdir.to_string_lossy();
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
