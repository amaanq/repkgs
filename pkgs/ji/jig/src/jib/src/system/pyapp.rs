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
   os::unix::fs::{
      PermissionsExt,
      symlink,
   },
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
use serde::{
   Deserialize,
   Serialize,
   Serializer,
};

use crate::{
   attrs::{
      Attrs,
      Ctx,
   },
   implant,
   prepare::succeed,
   schema::{
      Field,
      OptionType,
      PackagePath,
      Type,
   },
   system::{
      cargo::Cargo,
      command::{
         Env,
         run,
         unknown,
      },
      dispatch::System,
      python::{
         maturin_interpreter,
         tool_site_packages,
      },
      syslibs,
   },
};

options! {
   Options {
      root ("root"): String = ".".to_owned()
         => "directory below the source the project lives in (monorepos, build/cmake)",
      deps ("deps"): PackagePath
         => "the locked dependencies, fetched (fetch.pythonDeps) from the package's own lock file",
      check ("check"): Vec<String> = Vec::new()
         => "modules that must import from the installed layout",
      mode ("mode"): Mode = Mode::Project
         => "project installs the application, env and dev a virtual environment",
      overrides ("overrides"): BTreeMap<String, Override> = BTreeMap::new()
         => "per-package build options",
   }
}

#[derive(Clone, Copy, Deserialize, PartialEq)]
#[serde(try_from = "String")]
enum Mode {
   Project,
   Env,
   Dev,
}

impl Mode {
   const ALL: [Self; 3] = [Self::Project, Self::Env, Self::Dev];

   const fn name(self) -> &'static str {
      match self {
         Self::Project => "project",
         Self::Env => "env",
         Self::Dev => "dev",
      }
   }
}

impl TryFrom<String> for Mode {
   type Error = io::Error;

   fn try_from(value: String) -> io::Result<Self> {
      Self::ALL
         .into_iter()
         .find(|mode| mode.name() == value)
         .ok_or_else(|| io::Error::other(format!("unknown pyapp mode {value}")))
   }
}

impl Serialize for Mode {
   fn serialize<Encoder>(&self, serializer: Encoder) -> Result<Encoder::Ok, Encoder::Error>
   where
      Encoder: Serializer,
   {
      serializer.serialize_str(self.name())
   }
}

impl OptionType for Mode {
   fn kind() -> Type {
      Type::Enum {
         values: Self::ALL.map(|mode| mode.name().to_owned()).to_vec(),
      }
   }
}

record! {
   #[derive(Default)]
   Override {
      required {
         patches ("patches"): Vec<String>,
         env ("env"): BTreeMap<String, String>,
         run ("run"): String,
         config_settings ("configSettings"): BTreeMap<String, String>,
      }
   }
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum Kind {
   Wheel,
   Sdist,
   Tree,
}

#[derive(Deserialize)]
struct Planned {
   name: String,
   file: String,
   kind: Kind,
   path: String,
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

   fn options(&self) -> io::Result<Vec<Field>> {
      Options::schema(self.name())
   }

   // binary wheels carry upstream-linked .so files: finish implants
   // interp/RUNPATH like for any prebuilt package (after split-debug,
   // llvm-objcopy crashes on formatelf's layout)
   fn prebuilt(&self) -> bool {
      true
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
         "install" => install(ctx, &options),
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
   for sdist in plan
      .iter()
      .filter(|entry| matches!(entry.kind, Kind::Sdist))
   {
      build_sdist(
         ctx,
         attrs,
         options,
         &format!("{}/dist/{}", options.deps, sdist.file),
         &sdist.name,
         &built,
      )?;
   }
   for tree in plan.iter().filter(|entry| matches!(entry.kind, Kind::Tree)) {
      let source = if tree.file.is_empty() {
         format!("{}/{}", ctx.src, tree.path)
      } else {
         format!("{}/{}", options.deps, tree.file)
      };
      note("tree", &tree.name);
      let dir = format!("{}/sdist/{}", ctx.build, tree.name);

      if Path::new(&dir).exists() {
         remove_dir_all(&dir)?;
      }

      create_dir_all(format!("{}/sdist", ctx.build))?;
      run("cp", &["-rH", "--no-preserve=mode", &source, &dir], &[])?;
      build_wheel(ctx, attrs, options, &dir, &tree.name, &built)?;
   }

   let mut wheels: Vec<String> = plan
      .iter()
      .filter(|entry| matches!(entry.kind, Kind::Wheel))
      .map(|entry| format!("{}/dist/{}", options.deps, entry.file))
      .collect();
   wheels.extend(
      glob(&format!("{built}/*.whl"))?
         .iter()
         .map(|path| path.to_string_lossy().into_owned()),
   );
   for wheel in &wheels {
      install_wheel(ctx, wheel, options.mode != Mode::Project)?;
   }
   if options.mode == Mode::Project {
      let project = format!("{}/project", ctx.build);
      run("python3", &wheel_args(&project), &[])?;
      let own = glob(&format!("{project}/*.whl"))?
         .into_iter()
         .next()
         .ok_or_else(|| io::Error::other("python -m build produced no wheel"))?;
      install_wheel(ctx, &own.to_string_lossy(), true)?;
   } else {
      env_python(ctx, options.mode)?;
   }

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

fn build_sdist(
   ctx: &Ctx,
   attrs: &Attrs,
   options: &Options,
   tarball: &str,
   name: &str,
   outdir: &str,
) -> io::Result<()> {
   note("sdist", name);
   let dir = format!("{}/sdist/{name}", ctx.build);
   create_dir_all(&dir)?;
   run(
      "bsdtar",
      &["-xf", tarball, "-C", &dir, "--strip-components", "1"],
      &[],
   )?;
   build_wheel(ctx, attrs, options, &dir, name, outdir)
}

fn build_wheel(
   ctx: &Ctx,
   attrs: &Attrs,
   options: &Options,
   dir: &str,
   name: &str,
   outdir: &str,
) -> io::Result<()> {
   let back = current_dir()?;
   set_current_dir(dir)?;
   let built = (|| {
      let default_override = Override::default();
      let overrides = options.overrides.get(name).unwrap_or(&default_override);

      for patch in &overrides.patches {
         run("patch", &["-p1", "-i", patch], &[])?;
      }

      if !overrides.run.is_empty() {
         run("nu", &["-c", &overrides.run], &[])?;
      }

      let vendor = format!("{}/vendor/{name}", options.deps);
      let mut command = Command::new("python3");
      command.args(&wheel_args(outdir)[..7]);

      if Path::new(&vendor).exists() {
         let home = format!("{}/cargo-home/{name}", ctx.build);
         command.envs(Cargo::configure(ctx, attrs, Some(&vendor), &home)?);
         command.env_remove("RUSTFLAGS");
         command.env(
            "MATURIN_PEP517_ARGS",
            format!("--interpreter {} --auditwheel skip", maturin_interpreter()?),
         );
      }

      let tables = syslibs::tables(attrs)?;
      let mut settings = tables
         .get("python")
         .and_then(|table| table.get(name))
         .map(|entry| entry.config_settings.clone())
         .unwrap_or_default();
      settings.extend(
         overrides
            .config_settings
            .iter()
            .map(|(key, value)| (key.clone(), value.clone())),
      );
      command.args(
         settings
            .iter()
            .map(|(key, value)| format!("-C{key}={value}")),
      );
      succeed(command.arg(".").envs(&overrides.env), "python3 -m build")
   })();
   set_current_dir(back)?;
   built
}

fn env_python(ctx: &Ctx, mode: Mode) -> io::Result<()> {
   let python = ctx
      .deps
      .iter()
      .find(|dep| dep.name.starts_with("cpython"))
      .ok_or_else(|| io::Error::other("pkgs.cpython must be in dependencies"))?;
   let dev = if mode == Mode::Dev {
      "${REPKGS_PROJECT:+$REPKGS_PROJECT:$REPKGS_PROJECT/src:}"
   } else {
      ""
   };
   let bin = format!("{}/bin", ctx.out);
   create_dir_all(&bin)?;
   let wrapper = format!("{bin}/python3");
   let text = format!(
      r#"#!/bin/sh
here=$(cd "$(dirname "$0")" && pwd -P)
export PYTHONPATH="{dev}$here/../lib/{}/site-packages${{PYTHONPATH:+:$PYTHONPATH}}"
exec {}/bin/python3 "$@"
"#,
      ctx.spec()?.name,
      python.root
   );
   write_file(Path::new(&wrapper), text.as_bytes(), None)?;
   set_permissions(&wrapper, PermissionsExt::from_mode(0o755))?;
   symlink("python3", format!("{bin}/python"))
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
fn install(ctx: &Ctx, options: &Options) -> io::Result<()> {
   let python = ctx
      .deps
      .iter()
      .find(|dep| dep.name.starts_with("cpython"))
      .map(|dep| dep.root.clone())
      .ok_or_else(|| io::Error::other("pkgs.cpython must be in dependencies"))?;
   let dev = if options.mode == Mode::Dev {
      "; p = os.environ.get('REPKGS_PROJECT'); sys.path[0:0] = [p, os.path.join(p, 'src')] if p \
       else []"
   } else {
      ""
   };
   let prelude = format!(
      "import os, sys; sys.path.insert(0, \
       os.path.join(os.path.dirname(os.path.realpath(__file__)), '../lib/{}/site-packages')){dev}",
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
