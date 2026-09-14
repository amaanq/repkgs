use std::{
   env::var,
   fmt::Write as _,
   io,
   path::{
      Path,
      PathBuf,
   },
   process::{
      Command,
      Stdio,
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
   system::{
      project_dir,
      run,
      unknown,
   },
};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Options {
   root:       String,
   defs:       Map<String, Value>,
   flags:      Vec<String>,
   skip_tests: Vec<String>,
}

pub fn workdir(ctx: &Ctx) -> PathBuf {
   PathBuf::from(&ctx.build)
}

pub fn phase(ctx: &Ctx, verb: &str) -> io::Result<()> {
   let options: Options = ctx.options("meson")?;
   match verb {
      "configure" => configure(ctx, &options),
      "build" => run("ninja", &[format!("-j{}", ctx.njobs)], &[]),
      "test" => test(ctx, &options),
      "install" => {
         run(
            "meson",
            &["install".to_owned(), "--no-rebuild".to_owned()],
            &[],
         )
      },
      _ => Err(unknown("meson", verb)),
   }
}

fn literal(value: &Value) -> String {
   match *value {
      Value::String(ref text) => format!("'{text}'"),
      Value::Array(ref items) => {
         format!(
            "[{}]",
            items.iter().map(literal).collect::<Vec<_>>().join(", ")
         )
      },
      Value::Null | Value::Bool(_) | Value::Number(_) | Value::Object(_) => value.to_string(),
   }
}

fn machine_file(path: &str, sections: &[(&str, Vec<(&str, Value)>)]) -> io::Result<String> {
   let mut contents = String::new();
   for (index, section) in sections.iter().enumerate() {
      if index > 0 {
         contents.push('\n');
      }
      writeln!(contents, "[{}]", section.0).map_err(io::Error::other)?;
      for entry in &section.1 {
         writeln!(contents, "{} = {}", entry.0, literal(&entry.1)).map_err(io::Error::other)?;
      }
   }
   write_file(Path::new(path), contents.trim_end().as_bytes(), None)?;
   Ok(path.to_owned())
}

// the build machine's pkg-config finds nothing so build-time tools never link
// target libraries, and sizeof/alignment properties spare cc.sizeof() an exe
// wrapper
fn cross_files(ctx: &Ctx) -> io::Result<Vec<String>> {
   let platform = &ctx.platform;
   let os_names = platform.get("osNames").cloned().unwrap_or(Value::Null);
   let names = platform.get("names").cloned().unwrap_or(Value::Null);
   let emulator = words(platform, "emulator");
   let mut binaries = vec![
      ("c", json!("cc")),
      ("cpp", json!("c++")),
      ("ar", json!("llvm-ar")),
      ("nm", json!("llvm-nm")),
      ("strip", json!("llvm-strip")),
      ("objcopy", json!("llvm-objcopy")),
      ("pkg-config", json!("pkg-config")),
      ("cmake", json!("cmake")),
   ];
   if !emulator.is_empty() {
      binaries.push(("exe_wrapper", json!(emulator)));
   }
   let host = machine_file(&format!("{}/cross.ini", ctx.build), &[
      ("binaries", binaries),
      ("properties", vec![
         ("needs_exe_wrapper", json!(true)),
         ("sizeof_void_p", json!(8_u8)),
         ("sizeof_long", json!(8_u8)),
         ("sizeof_size_t", json!(8_u8)),
         ("alignment_void_p", json!(8_u8)),
         ("alignment_double", json!(8_u8)),
         ("sys_root", json!(var("PKGS_SYSROOT").unwrap_or_default())),
         (
            "pkg_config_libdir",
            json!(var("PKG_CONFIG_PATH").unwrap_or_default()),
         ),
      ]),
      ("host_machine", vec![
         ("system", json!(text(&os_names, "meson"))),
         ("kernel", json!(text(&os_names, "mesonKernel"))),
         ("cpu_family", json!(text(&names, "meson"))),
         ("cpu", json!(text(platform, "cpu"))),
         ("endian", json!("little")),
      ]),
   ])?;
   let native = machine_file(&format!("{}/native.ini", ctx.build), &[("binaries", vec![
      ("c", json!(var("CC_FOR_BUILD").unwrap_or_default())),
      ("cpp", json!(var("CXX_FOR_BUILD").unwrap_or_default())),
      ("ar", json!("llvm-ar")),
      ("strip", json!("llvm-strip")),
      (
         "pkg-config",
         json!(var("PKG_CONFIG_FOR_BUILD").unwrap_or_default()),
      ),
   ])])?;
   Ok(vec![
      "--cross-file".to_owned(),
      host,
      "--native-file".to_owned(),
      native,
   ])
}

fn render(value: &Value) -> String {
   match *value {
      Value::String(ref text) => text.clone(),
      Value::Null | Value::Bool(_) | Value::Number(_) | Value::Array(_) | Value::Object(_) => {
         value.to_string()
      },
   }
}

// buildtype plain because cc brings -O2 -g itself, auto_features disabled so
// packages enable what they declare
fn configure(ctx: &Ctx, options: &Options) -> io::Result<()> {
   let profile = ctx
      .spec
      .get("profile")
      .and_then(Value::as_str)
      .unwrap_or("release");
   let mut defs: Vec<(String, Value)> = vec![
      ("prefix".to_owned(), json!(ctx.out)),
      ("libdir".to_owned(), json!("lib")),
      (
         "buildtype".to_owned(),
         json!(if profile == "debug" { "debug" } else { "plain" }),
      ),
      ("default_library".to_owned(), json!("shared")),
      ("wrap_mode".to_owned(), json!("nodownload")),
      ("auto_features".to_owned(), json!("disabled")),
   ];
   for (name, value) in &options.defs {
      match defs.iter_mut().find(|entry| entry.0 == *name) {
         Some(entry) => entry.1 = value.clone(),
         None => defs.push((name.clone(), value.clone())),
      }
   }
   let mut args = vec![
      "setup".to_owned(),
      ".".to_owned(),
      project_dir(ctx, &options.root)
         .to_string_lossy()
         .into_owned(),
   ];
   if flag(&ctx.platform, "cross") {
      args.extend(cross_files(ctx)?);
   }
   args.extend(
      defs
         .iter()
         .map(|entry| format!("-D{}={}", entry.0, render(&entry.1))),
   );
   args.extend(options.flags.iter().cloned());
   run("meson", &args, &[])
}

fn test(ctx: &Ctx, options: &Options) -> io::Result<()> {
   let mut args = vec![
      "test".to_owned(),
      "--no-rebuild".to_owned(),
      "--print-errorlogs".to_owned(),
      "--num-processes".to_owned(),
      ctx.test_jobs()?.to_string(),
   ];
   if !options.skip_tests.is_empty() {
      let output = Command::new("meson")
         .args(["test", "--list"])
         .stderr(Stdio::inherit())
         .output()?;
      if !output.status.success() {
         return Err(io::Error::other("meson test --list failed"));
      }
      args.extend(
         String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter(|name| !options.skip_tests.iter().any(|skip| name.contains(skip)))
            .map(str::to_owned),
      );
   }
   run("meson", &args, &[])
}
