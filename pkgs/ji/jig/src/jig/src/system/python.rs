use std::{
   collections::BTreeMap,
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

use serde::{
   Deserialize,
   Serialize,
};

use crate::{
   attrs::{
      Attrs,
      Ctx,
   },
   exports::closure,
   files::write_file,
   store::STORE_DIR,
   system::command::{
      run,
      unknown,
   },
   tree::writable,
};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Options {
   root:    String,
   backend: String,
   #[serde(default)]
   module:  Option<String>,
   pytest:  bool,
}

#[derive(Serialize)]
struct Propagate<'deps> {
   propagate: &'deps [String],
}

pub fn workdir(ctx: &Ctx) -> io::Result<PathBuf> {
   let options: Options = ctx.options("python")?;
   Ok(ctx.project_dir(&options.root))
}

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

/// a PEP 517 backend imports its own dependencies, so the python tools on PATH
/// bring what they propagate.
fn tool_site_packages() -> io::Result<Vec<String>> {
   let mut roots = Vec::new();
   for directory in var("PATH").unwrap_or_default().split(':') {
      let root = Path::new(directory)
         .parent()
         .unwrap_or_else(|| Path::new("/"))
         .to_string_lossy()
         .into_owned();
      if !site_packages(slice::from_ref(&root))?.is_empty() {
         roots.push(root);
      }
   }
   let closure_roots: Vec<String> = closure(&roots)?.into_iter().map(|dep| dep.root).collect();
   site_packages(&closure_roots)
}

/// the source tree comes last so a backend can build itself before any of the
/// stack exists.
pub fn setup(ctx: &Ctx) -> io::Result<BTreeMap<String, String>> {
   let options: Options = ctx.options("python")?;
   let root = ctx.project_dir(&options.root);
   let dep_roots: Vec<String> = ctx.deps.iter().map(|dep| dep.root.clone()).collect();
   let mut path = site_packages(&dep_roots)?;
   path.extend(tool_site_packages()?);
   for own in [root.clone(), root.join("src")] {
      if own.exists() {
         path.push(own.to_string_lossy().into_owned());
      }
   }
   Ok(BTreeMap::from([
      ("PYTHONDONTWRITEBYTECODE".to_owned(), "1".to_owned()),
      ("PIP_NO_INDEX".to_owned(), "1".to_owned()),
      ("PYTHONPATH".to_owned(), path.join(":")),
   ]))
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

pub fn phase(ctx: &Ctx, verb: &str, attrs: &Attrs) -> io::Result<()> {
   let options: Options = ctx.options("python")?;
   match verb {
      "build" => build(ctx, &options),
      "install" => install(ctx, attrs),
      "test" => test(ctx, &options),
      _ => Err(unknown("python", verb)),
   }
}

/// maturin's PEP 517 backend only shells out to `maturin`, so call it directly.
fn build(ctx: &Ctx, options: &Options) -> io::Result<()> {
   let dist = format!("{}/dist", ctx.build);
   match options.backend.as_str() {
      "maturin" => {
         let version = python("import sys; print(\"%d.%d\" % sys.version_info[:2])")?;
         run(
            "maturin",
            &[
               "build".to_owned(),
               "--release".to_owned(),
               "--offline".to_owned(),
               format!("-j{}", ctx.njobs),
               "--interpreter".to_owned(),
               format!("python{version}"),
               "--auditwheel".to_owned(),
               "skip".to_owned(),
               "-o".to_owned(),
               dist,
            ],
            &[],
         )
      },
      "flit_core" => {
         run(
            "python3",
            &[
               "-m".to_owned(),
               "flit_core.wheel".to_owned(),
               "--outdir".to_owned(),
               dist,
               ".".to_owned(),
            ],
            &[],
         )
      },
      _ => {
         run(
            "python3",
            &[
               "-m".to_owned(),
               "build".to_owned(),
               "--wheel".to_owned(),
               "--no-isolation".to_owned(),
               "--skip-dependency-check".to_owned(),
               "--outdir".to_owned(),
               dist,
               ".".to_owned(),
            ],
            &[],
         )
      },
   }
}

/// entry-point scripts run under another package's interpreter and get
/// re-exec'd by path (meson --internal), so no env var will do, each script
/// puts our and our dependencies' site-packages on sys.path itself, relative to
/// its own location.
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
            "-m".to_owned(),
            "installer".to_owned(),
            "--prefix".to_owned(),
            ctx.out.clone(),
            "--no-compile-bytecode".to_owned(),
            wheel,
         ],
         &[],
      )?;
   } else {
      let version = python("import sys; print(f'{sys.version_info[0]}.{sys.version_info[1]}')")?;
      let site = format!("{}/lib/python{version}/site-packages", ctx.out);
      create_dir_all(&site)?;
      run(
         "bsdtar",
         &["-xf".to_owned(), wheel, "-C".to_owned(), site],
         &[],
      )?;
   }
   run(
      "python3",
      &[
         "-m".to_owned(),
         "compileall".to_owned(),
         "-q".to_owned(),
         "-o".to_owned(),
         "0".to_owned(),
         "-o".to_owned(),
         "1".to_owned(),
         "-s".to_owned(),
         ctx.out.clone(),
         format!("{}/lib", ctx.out),
      ],
      &[],
   )?;
   let mut roots = vec![ctx.out.clone()];
   roots.extend(ctx.deps.iter().map(|dep| dep.root.clone()));
   let store = var("NIX_STORE").unwrap_or_else(|_| STORE_DIR.to_owned());
   let rels: Vec<String> = site_packages(&roots)?
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
      "import os, sys; sys.path[0:0] = [os.path.join(os.path.dirname(os.path.realpath(__file__)), \
       p) for p in {}]",
      serde_json::to_string(&rels)?
   );
   boot_scripts(&Path::new(&ctx.out).join("bin"), &boot)?;
   // whoever depends on or runs this module gets our dependencies'
   // site-packages too, finish merges this into the final exports.json
   let mut rendered = serde_json::to_vec_pretty(&Propagate {
      propagate: &attrs.dependencies,
   })?;
   rendered.push(b'\n');
   write_file(&Path::new(&ctx.out).join("exports.json"), &rendered, None)
}

fn boot_scripts(bin: &Path, boot: &str) -> io::Result<()> {
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
      let mut fixed = vec![first.to_owned(), boot.to_owned()];
      fixed.extend(lines.map(str::to_owned));
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
   let mut path = site_packages(slice::from_ref(&ctx.out))?;
   path.push(var("PYTHONPATH").unwrap_or_default());
   let env = [("PYTHONPATH", path.join(":"))];
   set_current_dir(&ctx.build)?;
   run(
      "python3",
      &["-c".to_owned(), format!("import {module}")],
      &env,
   )?;
   if options.pytest {
      run(
         "python3",
         &[
            "-m".to_owned(),
            "pytest".to_owned(),
            "-q".to_owned(),
            format!("{}/tests", ctx.src),
         ],
         &env,
      )?;
   }
   Ok(())
}
