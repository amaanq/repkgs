use std::{
   env::{
      set_current_dir,
      var,
   },
   fs::{
      File,
      create_dir_all,
      read,
      symlink_metadata,
   },
   io::{
      self,
      Write as _,
   },
   path::Path,
   process::{
      Command,
      Stdio,
   },
   slice,
};

use mummer::{
   files::{
      sorted_entries,
      utf8,
      write_file,
   },
   process::Checked as _,
   tree::writable,
};
use serde::Serialize;

use crate::{
   attrs::{
      Attrs,
      Ctx,
      nix_store,
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
      let options = ctx.options::<Options>(self.name())?;
      let root = ctx.project_dir(&options.root);
      let dep_roots = ctx
         .deps
         .iter()
         .map(|dep| dep.root.clone())
         .collect::<Vec<String>>();
      let mut path = Self::site_packages(&dep_roots);
      path.extend(tool_site_packages()?);
      for own in [root.clone(), root.join("src")] {
         if own.exists() {
            path.push(utf8(&own)?.to_owned());
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
      let options = ctx.options::<Options>(self.name())?;
      match verb {
         "build" => Self::build(ctx, &options),
         "install" => Self::install(ctx, attrs),
         "test" => Self::test(ctx, &options),
         _ => Err(unknown(self.name(), verb)),
      }
   }
}

impl Python {
   fn site_packages(roots: &[String]) -> Vec<String> {
      let mut found = Vec::new();
      for root in roots {
         let Ok(versions) = sorted_entries(&Path::new(root).join("lib")) else {
            continue;
         };
         for version in versions {
            let site = version.join("site-packages");
            if version
               .file_name()
               .and_then(|name| name.to_str())
               .is_some_and(|name| name.starts_with("python3"))
               && site.is_dir()
               && let Some(text) = site.to_str()
            {
               found.push(text.to_owned());
            }
         }
      }
      found
   }

   fn python(code: &str) -> io::Result<String> {
      let printed = Command::new("python3")
         .args(["-c", code])
         .checked_output()?;
      Ok(String::from_utf8_lossy(&printed).trim().to_owned())
   }

   /// maturin's PEP 517 backend only shells out to `maturin`, so call it
   /// directly.
   fn build(ctx: &Ctx, options: &Options) -> io::Result<()> {
      let dist = format!("{}/dist", ctx.build.display());
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
      let dist = format!("{}/dist", ctx.build.display());
      let mut wheels = sorted_entries(Path::new(&dist))?;
      wheels.retain(|path| path.extension().is_some_and(|extension| extension == "whl"));
      let built = wheels
         .first()
         .ok_or_else(|| io::Error::other("no wheel in dist"))?;
      let wheel = utf8(built)?.to_owned();
      let out = utf8(&ctx.out)?;
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
               out,
               "--no-compile-bytecode",
               &wheel,
            ],
            &[],
         )?;
      } else {
         let version =
            Self::python("import sys; print(f'{sys.version_info[0]}.{sys.version_info[1]}')")?;
         let site = format!("{}/lib/python{version}/site-packages", ctx.out.display());
         create_dir_all(&site)?;
         run("bsdtar", &["-xf", &wheel, "-C", &site], &[])?;
      }
      let lib = format!("{}/lib", ctx.out.display());
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
            out,
            &lib,
         ],
         &[],
      )?;
      let mut roots = vec![out.to_owned()];
      roots.extend(ctx.deps.iter().map(|dep| dep.root.clone()));
      let store = nix_store();
      let rels = Self::site_packages(&roots)
         .iter()
         .map(|site| {
            site.strip_prefix(out).map_or_else(
               || {
                  format!(
                     "../../{}",
                     site.strip_prefix(&format!("{store}/")).unwrap_or(site)
                  )
               },
               |inside| format!("..{inside}"),
            )
         })
         .collect::<Vec<String>>();
      let boot = format!(
         "import os, sys; sys.path[0:0] = \
          [os.path.join(os.path.dirname(os.path.realpath(__file__)), p) for p in {}]",
         serde_json::to_string(&rels)?
      );
      Self::boot_scripts(&ctx.out.join("bin"), &boot, ctx.platform()?.cross)?;
      // whoever depends on or runs this module gets our dependencies'
      // site-packages too, finish merges this into the final exports.json
      let mut rendered = serde_json::to_vec_pretty(&Propagate {
         propagate: &attrs.dependencies,
      })?;
      rendered.push(b'\n');
      write_file(&ctx.out.join("exports.json"), &rendered, None)
   }

   fn boot_scripts(bin: &Path, boot: &str, cross: bool) -> io::Result<()> {
      if !bin.is_dir() {
         return Ok(());
      }
      for script in sorted_entries(bin)? {
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
      let mut path = Self::site_packages(&[utf8(&ctx.out)?.to_owned()]);
      path.push(var("PYTHONPATH").unwrap_or_default());
      let joined = path.join(":");
      let env = [("PYTHONPATH", joined.as_str())];
      set_current_dir(&ctx.build)?;
      run("python3", &["-c", &format!("import {module}")], &env)?;
      if options.pytest {
         let tests = format!("{}/tests", ctx.src.display());
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
      let root = utf8(
         Path::new(directory)
            .parent()
            .unwrap_or_else(|| Path::new("/")),
      )?
      .to_owned();
      if !Python::site_packages(slice::from_ref(&root)).is_empty() {
         roots.push(root);
      }
   }
   let closure_roots = closure(&roots)?
      .into_iter()
      .map(|dep| dep.root)
      .collect::<Vec<String>>();
   Ok(Python::site_packages(&closure_roots))
}

pub fn maturin_interpreter() -> io::Result<String> {
   let version = Python::python("import sys; print(\"%d.%d\" % sys.version_info[:2])")?;
   Ok(format!("python{version}"))
}
