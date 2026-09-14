use std::{
   io,
   path::{
      Path,
      PathBuf,
   },
};

use serde::Deserialize;
use serde_json::{
   Map,
   Value,
   json,
};

use crate::{
   attrs::{
      Ctx,
      flag,
      text,
      words,
   },
   files::write_file,
   probe,
   system::{
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
   defs:       Map<String, Value>,
   generator:  String,
   flags:      Vec<String>,
   skip_tests: Vec<String>,
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

fn render(value: &Value) -> String {
   match *value {
      Value::Bool(true) => "ON".to_owned(),
      Value::Bool(false) => "OFF".to_owned(),
      Value::String(ref text) => text.clone(),
      Value::Null | Value::Number(_) | Value::Array(_) | Value::Object(_) => value.to_string(),
   }
}

fn upsert(defs: &mut Vec<(String, Value)>, name: &str, value: Value) {
   match defs.iter_mut().find(|entry| entry.0 == name) {
      Some(entry) => entry.1 = value,
      None => defs.push((name.to_owned(), value)),
   }
}

/// the sysroot's include/ and lib/ for find_path/find_library (an Apple SDK has
/// them under usr/).
fn defaults(ctx: &Ctx, options: &Options) -> Vec<(String, Value)> {
   let platform = &ctx.platform;
   let sysroot = text(platform, "sysroot");
   let profile = ctx
      .spec
      .get("profile")
      .and_then(Value::as_str)
      .unwrap_or("release");
   let mut defs: Vec<(String, Value)> = vec![
      ("CMAKE_INSTALL_PREFIX".to_owned(), json!(ctx.out)),
      (
         "CMAKE_BUILD_TYPE".to_owned(),
         json!(if profile == "debug" {
            "Debug"
         } else {
            "Release"
         }),
      ),
      ("CMAKE_INSTALL_LIBDIR".to_owned(), json!("lib")),
      (
         "CMAKE_PREFIX_PATH".to_owned(),
         json!(
            ctx.deps
               .iter()
               .map(|dep| dep.root.as_str())
               .collect::<Vec<_>>()
               .join(";")
         ),
      ),
      ("CMAKE_SYSTEM_PREFIX_PATH".to_owned(), json!(sysroot)),
      ("BUILD_SHARED_LIBS".to_owned(), json!(true)),
      ("BUILD_TESTING".to_owned(), json!(ctx.tests_run)),
   ];
   if flag(platform, "cross") {
      upsert(
         &mut defs,
         "CMAKE_SYSTEM_NAME",
         json!(
            platform
               .get("osNames")
               .map(|names| text(names, "cmake"))
               .unwrap_or_default()
         ),
      );
      upsert(
         &mut defs,
         "CMAKE_SYSTEM_PROCESSOR",
         json!(text(platform, "cpu")),
      );
   }
   if text(platform, "os") == "macos" {
      upsert(&mut defs, "CMAKE_OSX_SYSROOT", json!(sysroot));
   }
   let emulator = words(platform, "emulator");
   if !emulator.is_empty() {
      upsert(
         &mut defs,
         "CMAKE_CROSSCOMPILING_EMULATOR",
         json!(emulator.join(";")),
      );
   }
   for (name, value) in &options.defs {
      upsert(&mut defs, name, value.clone());
   }
   defs
}

fn configure(ctx: &Ctx, options: &Options, socket: &Path) -> io::Result<()> {
   let defs = defaults(ctx, options);
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
   let rendered: Map<String, Value> = defs
      .iter()
      .map(|entry| (entry.0.clone(), entry.1.clone()))
      .collect();
   let key = probe::key(
      "cmake",
      &scripts,
      Some(&json!({
         "defs": rendered,
         "generator": options.generator,
         "flags": options.flags,
      })),
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
