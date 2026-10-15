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

use mummer::{
   files::{
      glob,
      utf8,
      write_file,
   },
   syslibs::Ecosystem,
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
      nix_store,
   },
   exports::{
      Exports,
      relative,
   },
   implant,
   launchers::entries::{
      Record,
      Variable,
      place,
      script_env,
   },
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

fn cpython(ctx: &Ctx) -> io::Result<&Exports> {
   ctx.deps
      .iter()
      .find(|dep| dep.name.starts_with("cpython"))
      .ok_or_else(|| io::Error::other("pkgs.cpython must be in dependencies"))
}

fn site_packages(ctx: &Ctx) -> io::Result<String> {
   Ok(format!(
      "{}/lib/{}/site-packages",
      ctx.out.display(),
      ctx.spec()?.name
   ))
}

pub struct Pyapp;

impl System for Pyapp {
   fn module(&self) -> &'static str {
      module_path!()
   }

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
      env.extend(syslibs::active(attrs, Ecosystem::Python, &ctx.deps).env);
      Ok(env.into())
   }

   fn phase(&self, ctx: &Ctx, attrs: &Attrs, verb: &str, _socket: &Path) -> io::Result<()> {
      let options = ctx.options::<Options>(self.name())?;
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
         _ => Err(unknown(self.name(), verb)),
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
   let built = format!("{}/wheels", ctx.build.display());
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
         format!("{}/{}", ctx.src.display(), tree.path)
      } else {
         format!("{}/{}", options.deps, tree.file)
      };
      note("tree", &tree.name);
      let dir = format!("{}/sdist/{}", ctx.build.display(), tree.name);

      if Path::new(&dir).exists() {
         remove_dir_all(&dir)?;
      }

      create_dir_all(format!("{}/sdist", ctx.build.display()))?;
      run("cp", &["-rH", "--no-preserve=mode", &source, &dir], &[])?;
      build_wheel(ctx, attrs, options, &dir, &tree.name, &built)?;
   }

   let mut wheels = plan
      .iter()
      .filter(|entry| matches!(entry.kind, Kind::Wheel))
      .map(|entry| format!("{}/dist/{}", options.deps, entry.file))
      .collect::<Vec<String>>();
   wheels.extend(
      glob(&format!("{built}/*.whl"))?
         .iter()
         .map(|path| Ok(utf8(path)?.to_owned()))
         .collect::<io::Result<Vec<_>>>()?,
   );
   for wheel in &wheels {
      install_wheel(ctx, wheel, options.mode != Mode::Project)?;
   }
   if options.mode == Mode::Project {
      let project = format!("{}/project", ctx.build.display());
      run("python3", &wheel_args(&project), &[])?;
      let own = glob(&format!("{project}/*.whl"))?
         .into_iter()
         .next()
         .ok_or_else(|| io::Error::other("python -m build produced no wheel"))?;
      install_wheel(ctx, utf8(&own)?, true)?;
   } else {
      env_python(ctx, attrs, options.mode)?;
   }

   let platform = ctx.platform()?;
   implant::run(
      ctx.out.as_path(),
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
   let dir = format!("{}/sdist/{name}", ctx.build.display());
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
         let home = format!("{}/cargo-home/{name}", ctx.build.display());
         command.envs(Cargo::configure(ctx, attrs, Some(&vendor), &home)?);
         command.env_remove("RUSTFLAGS");
         command.env(
            "MATURIN_PEP517_ARGS",
            format!("--interpreter {} --auditwheel skip", maturin_interpreter()?),
         );
      }

      let mut settings = attrs
         .sys_libs
         .values()
         .filter_map(|bindings| bindings.get(&Ecosystem::Python))
         .find_map(|binding| binding.config_settings.get(name))
         .cloned()
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
      succeed(command.arg(".").envs(&overrides.env))
   })();
   set_current_dir(back)?;
   built
}

fn env_python(ctx: &Ctx, attrs: &Attrs, mode: Mode) -> io::Result<()> {
   let python = cpython(ctx)?;
   let mut path = Vec::new();
   if mode == Mode::Dev {
      path.extend(["{env:MOTLEY_PROJECT}", "{env:MOTLEY_PROJECT}/src"].map(str::to_owned));
   }
   path.push(format!("{{root}}/lib/{}/site-packages", ctx.spec()?.name));
   let mut env = script_env(ctx, &attrs.dependencies)?;
   if env
      .insert("PYTHONPATH".to_owned(), Variable::Prepend(path))
      .is_some()
   {
      return Err(io::Error::other("a dependency exports PYTHONPATH"));
   }
   let program = relative(
      &format!("{}/bin/python3", python.root),
      utf8(&ctx.out)?,
      &nix_store(),
   );
   place(ctx, "python3", &Record {
      env: &env,
      program,
      args: None,
      after: Vec::new(),
      argv0: None,
   })?;
   symlink("python3", ctx.out.join("bin/python"))
}

/// installer only knows prefix layouts, so the wheel is staged and moved.
/// Dependency scripts are dropped, the project's own land in $out/bin.
fn install_wheel(ctx: &Ctx, wheel: &str, scripts: bool) -> io::Result<()> {
   let stage = format!("{}/stage", ctx.build.display());
   if symlink_metadata(&stage).is_ok() {
      remove_dir_all(&stage)?;
   }
   succeed(Command::new("python3").args([
      "-m",
      "installer",
      "--prefix",
      &stage,
      "--no-compile-bytecode",
      wheel,
   ]))?;
   let site = site_packages(ctx)?;
   for packages in glob(&format!("{stage}/lib/python3*/site-packages"))? {
      succeed(Command::new("cp").args(["-rlf", &format!("{}/.", utf8(&packages)?), &site]))?;
      remove_dir_all(&packages)?;
   }
   let bin = format!("{stage}/bin");
   if scripts && Path::new(&bin).exists() {
      let out_bin = format!("{}/bin", ctx.out.display());
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
   let python = &cpython(ctx)?.root;
   let dev = if options.mode == Mode::Dev {
      "; p = os.environ.get('MOTLEY_PROJECT'); sys.path[0:0] = [p, os.path.join(p, 'src')] if p \
       else []"
   } else {
      ""
   };
   let prelude = format!(
      "import os, sys; sys.path.insert(0, \
       os.path.join(os.path.dirname(os.path.realpath(__file__)), '../lib/{}/site-packages')){dev}",
      ctx.spec()?.name
   );
   for script in glob(&format!("{}/bin/*", ctx.out.display()))? {
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
