use std::{
   collections::BTreeMap,
   env::{
      current_dir,
      set_current_dir,
   },
   fs::{
      create_dir_all,
      metadata,
      read_to_string,
      remove_dir_all,
      rename,
      set_permissions,
      symlink_metadata,
   },
   io,
   os::unix::fs::PermissionsExt,
   path::Path,
   process::Command,
};

use jig::{
   files::{
      glob,
      write_file,
   },
   tree::note,
};
use serde::Deserialize;

use crate::{
   attrs::{
      Attrs,
      Ctx,
   },
   implant,
   prepare::succeed,
   system::{
      command::{
         Env,
         run,
         unknown,
      },
      dispatch::System,
      python::tool_site_packages,
      syslibs,
   },
};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Options {
   root:  String,
   deps:  String,
   check: Vec<String>,
}

#[derive(Deserialize)]
struct Planned {
   name: String,
   file: String,
   kind: String,
}

fn site_packages(ctx: &Ctx) -> io::Result<String> {
   Ok(format!(
      "{}/lib/{}/site-packages",
      ctx.out,
      ctx.spec()?.name
   ))
}

pub struct Pyapp;

impl System for Pyapp {
   fn name(&self) -> &'static str {
      "pyapp"
   }

   /// PYTHONPATH is the application's site-packages, filling up during the
   /// build, then the build backends.
   fn setup(&self, ctx: &Ctx, attrs: &Attrs, _socket: &Path) -> io::Result<Env> {
      let options: Options = ctx.options("pyapp")?;
      let site = site_packages(ctx)?;
      create_dir_all(&site)?;
      let mut path = vec![site];
      path.extend(tool_site_packages()?);
      let mut env = BTreeMap::from([
         ("PYTHONPATH".to_owned(), path.join(":")),
         ("PYTHONDONTWRITEBYTECODE".to_owned(), "1".to_owned()),
         ("PYTHONNOUSERSITE".to_owned(), "1".to_owned()),
         ("PIP_NO_INDEX".to_owned(), "1".to_owned()),
      ]);
      syslibs::check(
         attrs,
         &ctx.project_dir(&options.root),
         ctx.spec()?.sys.as_deref(),
      )?;
      env.extend(syslibs::active(attrs, "python", &ctx.deps)?.env);
      Ok(env.into())
   }

   fn phase(&self, ctx: &Ctx, attrs: &Attrs, verb: &str, _socket: &Path) -> io::Result<()> {
      let options: Options = ctx.options("pyapp")?;
      match verb {
         "build" => build(ctx, attrs, &options),
         "test" => {
            let site = site_packages(ctx)?;
            for module in &options.check {
               let check = format!("import {module}");
               run("python3", &["-c", &check], &[("PYTHONPATH", site.as_str())])?;
            }
            Ok(())
         },
         "install" => install(ctx),
         _ => Err(unknown("pyapp", verb)),
      }
   }
}

/// wheels are unpacked as fetched, sdists built here with no build isolation,
/// so their PEP 517 backends come from buildDependencies. The test phase
/// imports the foreign ELFs before finish would get to them, so they are
/// implanted here.
fn build(ctx: &Ctx, attrs: &Attrs, options: &Options) -> io::Result<()> {
   let plan: Vec<Planned> =
      serde_json::from_str(&read_to_string(format!("{}/plan.json", options.deps))?)?;
   let built = format!("{}/wheels", ctx.build);
   create_dir_all(&built)?;
   for sdist in plan.iter().filter(|entry| entry.kind == "sdist") {
      build_sdist(
         ctx,
         &format!("{}/dist/{}", options.deps, sdist.file),
         &sdist.name,
         &built,
      )?;
   }
   let mut wheels: Vec<String> = plan
      .iter()
      .filter(|entry| entry.kind == "wheel")
      .map(|entry| format!("{}/dist/{}", options.deps, entry.file))
      .collect();
   wheels.extend(
      glob(&format!("{built}/*.whl"))?
         .iter()
         .map(|path| path.to_string_lossy().into_owned()),
   );
   for wheel in &wheels {
      install_wheel(ctx, wheel, false)?;
   }
   let project = format!("{}/project", ctx.build);
   run("python3", &wheel_args(&project), &[])?;
   let own = glob(&format!("{project}/*.whl"))?
      .into_iter()
      .next()
      .ok_or_else(|| io::Error::other("python -m build produced no wheel"))?;
   install_wheel(ctx, &own.to_string_lossy(), true)?;
   let platform = ctx.platform()?;
   implant::run(
      Path::new(&ctx.out),
      Path::new(&platform.interp),
      Some(Path::new(&platform.reloc_stub)),
      &attrs.dependencies,
   )
}

const fn wheel_args(outdir: &str) -> [&str; 8] {
   [
      "-m",
      "build",
      "--wheel",
      "--no-isolation",
      "--skip-dependency-check",
      "--outdir",
      outdir,
      ".",
   ]
}

fn build_sdist(ctx: &Ctx, tarball: &str, name: &str, outdir: &str) -> io::Result<()> {
   note("sdist", name);
   let dir = format!("{}/sdist/{name}", ctx.build);
   create_dir_all(&dir)?;
   run(
      "bsdtar",
      &["-xf", tarball, "-C", &dir, "--strip-components", "1"],
      &[],
   )?;
   let back = current_dir()?;
   set_current_dir(&dir)?;
   let built = run("python3", &wheel_args(outdir), &[]);
   set_current_dir(back)?;
   built
}

/// installer only knows prefix layouts, so the wheel is staged and moved.
/// Dependency scripts are dropped, the project's own land in $out/bin.
fn install_wheel(ctx: &Ctx, wheel: &str, scripts: bool) -> io::Result<()> {
   let stage = format!("{}/stage", ctx.build);
   if symlink_metadata(&stage).is_ok() {
      remove_dir_all(&stage)?;
   }
   succeed(
      Command::new("python3").args([
         "-m",
         "installer",
         "--prefix",
         &stage,
         "--no-compile-bytecode",
         wheel,
      ]),
      "installer",
   )?;
   let site = site_packages(ctx)?;
   for packages in glob(&format!("{stage}/lib/python3*/site-packages"))? {
      succeed(
         Command::new("cp").args(["-rlf", &format!("{}/.", packages.to_string_lossy()), &site]),
         "cp",
      )?;
      remove_dir_all(&packages)?;
   }
   let bin = format!("{stage}/bin");
   if scripts && Path::new(&bin).exists() {
      let out_bin = format!("{}/bin", ctx.out);
      create_dir_all(&out_bin)?;
      for script in glob(&format!("{bin}/*"))? {
         let name = script.file_name().unwrap_or_default().to_owned();
         rename(&script, Path::new(&out_bin).join(name))?;
      }
   }
   Ok(())
}

/// installer wrote the entry points with a `#!python3` placeholder, so they get
/// our interpreter and the application's site-packages first on sys.path,
/// relative to the script itself.
fn install(ctx: &Ctx) -> io::Result<()> {
   let python = ctx
      .deps
      .iter()
      .find(|dep| dep.name.starts_with("cpython"))
      .map(|dep| dep.root.clone())
      .ok_or_else(|| io::Error::other("pkgs.cpython must be in dependencies"))?;
   let prelude = format!(
      "import os, sys; sys.path.insert(0, \
       os.path.join(os.path.dirname(os.path.realpath(__file__)), '../lib/{}/site-packages'))",
      ctx.spec()?.name
   );
   for script in glob(&format!("{}/bin/*", ctx.out))? {
      if symlink_metadata(&script)?.is_symlink() {
         continue;
      }
      let text = read_to_string(&script)?;
      let mut lines = text.lines();
      let Some(first) = lines.next() else {
         continue;
      };
      if !(first.starts_with("#!") && first.contains("python")) {
         continue;
      }
      let mut rewritten = vec![format!("#!{python}/bin/python3"), prelude.clone()];
      rewritten.extend(lines.map(str::to_owned));
      write_file(&script, rewritten.join("\n").as_bytes(), None)?;
      let mode = metadata(&script)?.permissions().mode();
      set_permissions(&script, PermissionsExt::from_mode(mode | 0o111))?;
   }
   Ok(())
}
