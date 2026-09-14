use std::{
   collections::BTreeMap,
   io,
   path::{
      Path,
      PathBuf,
   },
};

use serde::{
   Deserialize,
   Serialize,
};

use crate::{
   attrs::{
      Ctx,
      Profile,
   },
   files::write_file,
   probe,
   system::{
      Def,
      run,
      unknown,
   },
   tree::{
      note,
      walk,
   },
};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Options {
   root:       String,
   defs:       BTreeMap<String, Def>,
   generator:  String,
   flags:      Vec<String>,
   skip_tests: Vec<String>,
}

#[derive(Serialize)]
struct Extra<'options> {
   defs:      BTreeMap<&'options str, &'options Def>,
   generator: &'options str,
   flags:     &'options [String],
}

pub fn workdir(ctx: &Ctx) -> PathBuf {
   PathBuf::from(&ctx.build)
}

pub fn phase(ctx: &Ctx, verb: &str, socket: &Path) -> io::Result<()> {
   let options: Options = ctx.options("cmake")?;
   match verb {
      "configure" => configure(ctx, &options, socket),
      "build" => {
         run(
            "cmake",
            &[
               "--build".to_owned(),
               ".".to_owned(),
               format!("-j{}", ctx.njobs),
            ],
            &[],
         )
      },
      "test" => {
         let mut args = vec![
            "--output-on-failure".to_owned(),
            "-j".to_owned(),
            ctx.test_jobs()?.to_string(),
         ];
         if !options.skip_tests.is_empty() {
            args.push("-E".to_owned());
            args.push(options.skip_tests.join("|"));
         }
         run("ctest", &args, &[])
      },
      "install" => run("cmake", &["--install".to_owned(), ".".to_owned()], &[]),
      _ => Err(unknown("cmake", verb)),
   }
}

fn render(def: &Def) -> String {
   match *def {
      Def::Flag(true) => "ON".to_owned(),
      Def::Flag(false) => "OFF".to_owned(),
      Def::Text(ref text) => text.clone(),
      Def::Number(number) => number.to_string(),
   }
}

fn upsert(defs: &mut Vec<(String, Def)>, name: &str, value: Def) {
   match defs.iter_mut().find(|entry| entry.0 == name) {
      Some(entry) => entry.1 = value,
      None => defs.push((name.to_owned(), value)),
   }
}

/// Darwin.cmake finds usr/ and the frameworks from `CMAKE_OSX_SYSROOT`.
fn defaults(ctx: &Ctx, options: &Options) -> io::Result<Vec<(String, Def)>> {
   let platform = ctx.platform()?;
   let build_type = match ctx.spec()?.profile {
      Profile::Debug => "Debug",
      Profile::Release => "Release",
   };
   let mut defs = vec![
      (
         "CMAKE_INSTALL_PREFIX".to_owned(),
         Def::Text(ctx.out.clone()),
      ),
      (
         "CMAKE_BUILD_TYPE".to_owned(),
         Def::Text(build_type.to_owned()),
      ),
      (
         "CMAKE_INSTALL_LIBDIR".to_owned(),
         Def::Text("lib".to_owned()),
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

fn configure(ctx: &Ctx, options: &Options, socket: &Path) -> io::Result<()> {
   let defs = defaults(ctx, options)?;
   let srcdir = ctx.project_dir(&options.root);
   let scripts: Vec<String> = walk(&srcdir)?
      .into_iter()
      .filter(|entry| {
         entry.kind == 'f'
            && (entry.rel.ends_with("CMakeLists.txt")
               || Path::new(&entry.rel)
                  .extension()
                  .is_some_and(|extension| extension.eq_ignore_ascii_case("cmake")))
      })
      .map(|entry| entry.path)
      .collect();
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
      false,
      socket,
   )?;
   let init = format!("{}/probe-init.cmake", ctx.build);
   let restored = probe::restore(&key, Path::new(&init), socket)?;
   note("cmake-probes", if restored { "restored" } else { "cold" });
   let mut args = vec![
      "-S".to_owned(),
      srcdir.to_string_lossy().into_owned(),
      "-B".to_owned(),
      ".".to_owned(),
      "-G".to_owned(),
      options.generator.clone(),
   ];
   if restored {
      args.push("-C".to_owned());
      args.push(init.clone());
   }
   args.extend(
      defs
         .iter()
         .map(|entry| format!("-D{}={}", entry.0, render(&entry.1))),
   );
   args.extend(options.flags.iter().cloned());
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
