use std::{
   env::{
      set_current_dir,
      var,
   },
   fs::{
      File,
      create_dir_all,
      read,
      read_dir,
      symlink_metadata,
   },
   io::{
      self,
      Write as _,
   },
   path::{
      Path,
      PathBuf,
   },
   process::{
      Command,
      Stdio,
   },
   slice,
};

use mummer::{
   files::write_file,
   store::STORE_DIR,
   tree::writable,
};
use serde::Serialize;

use crate::{
   attrs::{
      Attrs,
      Ctx,
   },
   exports::closure,
   schema::Field,
   system::{
      command::{
         Env,
         run,
         unknown,
      },
      dispatch::System,
   },
};

options! {
   Options {
      root ("root"): String = ".".to_owned()
         => "directory below the source the project lives in (monorepos, build/cmake)",
      backend ("backend"): String = "setuptools".to_owned()
         => "PEP 517 backend when pyproject.toml names none, setuptools, flit_core, hatchling or maturin",
      module ("module"): Option<String> = None
         => "module the import test loads (null uses the package name with - as _)",
      pytest ("pytest"): bool = false
         => "also run pytest on tests/",
   }
}

#[derive(Serialize)]
struct Propagate<'deps> {
   propagate: &'deps [String],
}

pub struct Python;

impl System for Python {
   fn name(&self) -> &'static str {
      "python"
   }

   fn options(&self) -> io::Result<Vec<Field>> {
      Options::schema(self.name())
   }

   // tests import the installed module
   fn phases(&self) -> &'static [&'static str] {
      &["build", "install", "test"]
   }

   /// the source tree comes last so a backend can build itself before any of
   /// the stack exists.
   fn setup(&self, ctx: &Ctx, _attrs: &Attrs, _socket: &Path) -> io::Result<Env> {
      let options: Options = ctx.options("python")?;
      let root = ctx.project_dir(&options.root);
      let dep_roots: Vec<String> = ctx.deps.iter().map(|dep| dep.root.clone()).collect();
      let mut path = Self::site_packages(&dep_roots)?;
      path.extend(tool_site_packages()?);
      for own in [root.clone(), root.join("src")] {
         if own.exists() {
            path.push(own.to_string_lossy().into_owned());
         }
      }
      Ok([
         ("PYTHONDONTWRITEBYTECODE", "1".to_owned()),
         ("PIP_NO_INDEX", "1".to_owned()),
         ("PYTHONPATH", path.join(":")),
      ]
      .into())
   }

   fn phase(&self, ctx: &Ctx, attrs: &Attrs, verb: &str, _socket: &Path) -> io::Result<()> {
      let options: Options = ctx.options("python")?;
      match verb {
         "build" => Self::build(ctx, &options),
         "install" => Self::install(ctx, attrs),
         "test" => Self::test(ctx, &options),
         _ => Err(unknown("python", verb)),
      }
   }
}

impl Python {
   fn site_packages(roots: &[String]) -> io::Result<Vec<String>> {
      let mut found = Vec::new();
      for root in roots {
         let Ok(entries) = read_dir(Path::new(root).join("lib")) else {
            continue;
         };
         let mut versions: Vec<PathBuf> = entries
            .map(|entry| entry.map(|listed| listed.path()))
            .collect::<io::Result<_>>()?;
         versions.sort();
         for version in versions {
            let name = version.file_name().unwrap_or_default().to_string_lossy();
            let site = version.join("site-packages");
            if name.starts_with("python3") && site.is_dir() {
               found.push(site.to_string_lossy().into_owned());
            }
         }
      }
      Ok(found)
   }

   fn python(code: &str) -> io::Result<String> {
      let output = Command::new("python3")
         .args(["-c", code])
         .stderr(Stdio::inherit())
         .output()?;
      if !output.status.success() {
         return Err(io::Error::other(format!("python3 -c {code} failed")));
      }
      Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
   }

   /// maturin's PEP 517 backend only shells out to `maturin`, so call it
   /// directly.
   fn build(ctx: &Ctx, options: &Options) -> io::Result<()> {
      let dist = format!("{}/dist", ctx.build);
      match options.backend.as_str() {
         "maturin" => {
            let jobs = format!("-j{}", ctx.njobs);
            let interpreter = maturin_interpreter()?;
            run(
               "maturin",
               &[
                  "build",
                  "--release",
                  "--offline",
                  &jobs,
                  "--interpreter",
                  &interpreter,
                  "--auditwheel",
                  "skip",
                  "-o",
                  &dist,
               ],
               &[],
            )
         },
         "flit_core" => {
            run(
               "python3",
               &["-m", "flit_core.wheel", "--outdir", &dist, "."],
               &[],
            )
         },
         _ => {
            run(
               "python3",
               &[
                  "-m",
                  "build",
                  "--wheel",
                  "--no-isolation",
                  "--skip-dependency-check",
                  "--outdir",
                  &dist,
                  ".",
               ],
               &[],
            )
         },
      }
   }

   /// entry-point scripts run under another package's interpreter and get
   /// re-exec'd by path (meson --internal), so no env var will do, each script
   /// puts our and our dependencies' site-packages on sys.path itself, relative
   /// to its own location.
   fn install(ctx: &Ctx, attrs: &Attrs) -> io::Result<()> {
      let dist = format!("{}/dist", ctx.build);
      let mut wheels: Vec<PathBuf> = read_dir(&dist)?
         .map(|entry| entry.map(|found| found.path()))
         .collect::<io::Result<_>>()?;
      wheels.retain(|path| path.extension().is_some_and(|extension| extension == "whl"));
      wheels.sort();
      let wheel = wheels
         .first()
         .ok_or_else(|| io::Error::other("no wheel in dist"))?
         .to_string_lossy()
         .into_owned();
      let has_installer = Command::new("python3")
         .args(["-c", "import installer"])
         .stdout(Stdio::null())
         .stderr(Stdio::null())
         .status()?
         .success();
      if has_installer {
         run(
            "python3",
            &[
               "-m",
               "installer",
               "--prefix",
               &ctx.out,
               "--no-compile-bytecode",
               &wheel,
            ],
            &[],
         )?;
      } else {
         let version =
            Self::python("import sys; print(f'{sys.version_info[0]}.{sys.version_info[1]}')")?;
         let site = format!("{}/lib/python{version}/site-packages", ctx.out);
         create_dir_all(&site)?;
         run("bsdtar", &["-xf", &wheel, "-C", &site], &[])?;
      }
      let lib = format!("{}/lib", ctx.out);
      run(
         "python3",
         &[
            "-m",
            "compileall",
            "-q",
            "-o",
            "0",
            "-o",
            "1",
            "-s",
            &ctx.out,
            &lib,
         ],
         &[],
      )?;
      let mut roots = vec![ctx.out.clone()];
      roots.extend(ctx.deps.iter().map(|dep| dep.root.clone()));
      let store = var("NIX_STORE").unwrap_or_else(|_| STORE_DIR.to_owned());
      let rels: Vec<String> = Self::site_packages(&roots)?
         .iter()
         .map(|site| {
            site.strip_prefix(&ctx.out).map_or_else(
               || {
                  format!(
                     "../../{}",
                     site.strip_prefix(&format!("{store}/")).unwrap_or(site)
                  )
               },
               |inside| format!("..{inside}"),
            )
         })
         .collect();
      let boot = format!(
         "import os, sys; sys.path[0:0] = \
          [os.path.join(os.path.dirname(os.path.realpath(__file__)), p) for p in {}]",
         serde_json::to_string(&rels)?
      );
      Self::boot_scripts(
         &Path::new(&ctx.out).join("bin"),
         &boot,
         ctx.platform()?.cross,
      )?;
      // whoever depends on or runs this module gets our dependencies'
      // site-packages too, finish merges this into the final exports.json
      let mut rendered = serde_json::to_vec_pretty(&Propagate {
         propagate: &attrs.dependencies,
      })?;
      rendered.push(b'\n');
      write_file(&Path::new(&ctx.out).join("exports.json"), &rendered, None)
   }

   fn boot_scripts(bin: &Path, boot: &str, cross: bool) -> io::Result<()> {
      if !bin.is_dir() {
         return Ok(());
      }
      let mut scripts: Vec<PathBuf> = read_dir(bin)?
         .map(|entry| entry.map(|found| found.path()))
         .collect::<io::Result<_>>()?;
      scripts.sort();
      for script in scripts {
         if !symlink_metadata(&script)?.is_file() {
            continue;
         }
         let bytes = read(&script)?;
         if !bytes.starts_with(b"#!") {
            continue;
         }
         let text = String::from_utf8_lossy(&bytes);
         let mut lines = text.lines();
         let Some(first) = lines.next() else {
            continue;
         };
         if !first.contains("python") {
            continue;
         }
         let interp = if cross {
            "#!/usr/bin/env python3"
         } else {
            first
         };
         let mut fixed = vec![interp, boot];
         fixed.extend(lines);
         writable(&script)?;
         File::create(&script)?.write_all(fixed.join("\n").as_bytes())?;
      }
      Ok(())
   }

   /// runs after install, imports from $out, not from the source tree.
   fn test(ctx: &Ctx, options: &Options) -> io::Result<()> {
      let spec = ctx.spec()?;
      let module = options
         .module
         .clone()
         .unwrap_or_else(|| spec.name.replace('-', "_"));
      let mut path = Self::site_packages(slice::from_ref(&ctx.out))?;
      path.push(var("PYTHONPATH").unwrap_or_default());
      let joined = path.join(":");
      let env = [("PYTHONPATH", joined.as_str())];
      set_current_dir(&ctx.build)?;
      run("python3", &["-c", &format!("import {module}")], &env)?;
      if options.pytest {
         let tests = format!("{}/tests", ctx.src);
         run("python3", &["-m", "pytest", "-q", &tests], &env)?;
      }
      Ok(())
   }
}

/// a PEP 517 backend imports its own dependencies, so the python tools on PATH
/// bring what they propagate.
pub fn tool_site_packages() -> io::Result<Vec<String>> {
   let mut roots = Vec::new();
   for directory in var("PATH").unwrap_or_default().split(':') {
      let root = Path::new(directory)
         .parent()
         .unwrap_or_else(|| Path::new("/"))
         .to_string_lossy()
         .into_owned();
      if !Python::site_packages(slice::from_ref(&root))?.is_empty() {
         roots.push(root);
      }
   }
   let closure_roots: Vec<String> = closure(&roots)?.into_iter().map(|dep| dep.root).collect();
   Python::site_packages(&closure_roots)
}

pub fn maturin_interpreter() -> io::Result<String> {
   let version = Python::python("import sys; print(\"%d.%d\" % sys.version_info[:2])")?;
   Ok(format!("python{version}"))
}
