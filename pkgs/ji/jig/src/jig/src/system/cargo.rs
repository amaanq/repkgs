use std::{
   collections::BTreeMap,
   env::var,
   fmt::Write as _,
   fs::{
      copy,
      create_dir_all,
      read_dir,
      remove_file,
      symlink_metadata,
   },
   io,
   os::unix::fs::symlink,
   path::{
      Path,
      PathBuf,
   },
   process::Command,
};

use serde::Deserialize;

use crate::{
   attrs::{
      Attrs,
      Ctx,
   },
   exports::exports_of,
   files::write_file,
   prepare::succeed,
   store::on_path,
   system::{
      command::{
         run,
         tool,
         unknown,
      },
      syslibs,
   },
   tree::note,
};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Options {
   root:                String,
   features:            Vec<String>,
   no_default_features: bool,
   flags:               Vec<String>,
   skip_tests:          Vec<String>,
   #[serde(default)]
   deps:                Option<String>,
   #[serde(default)]
   crate_patches:       BTreeMap<String, Vec<String>>,
}

fn arguments(options: &Options) -> Vec<String> {
   let mut args = Vec::new();
   if options.no_default_features {
      args.push("--no-default-features".to_owned());
   }
   if !options.features.is_empty() {
      args.push(format!("--features={}", options.features.join(",")));
   }
   args.extend(options.flags.iter().cloned());
   args
}

pub fn workdir(ctx: &Ctx) -> io::Result<PathBuf> {
   let options: Options = ctx.options("cargo")?;
   Ok(ctx.project_dir(&options.root))
}

fn quoted(value: &str) -> String {
   format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

fn list(values: &[String]) -> String {
   let items: Vec<String> = values.iter().map(|value| quoted(value)).collect();
   format!("[{}]", items.join(", "))
}

/// pkg-config, the -sys crates' usual probe, refuses to answer under --target
/// without `ALLOW_CROSS`. bindgen asks a clang executable for its include dirs,
/// ours and not a bare one on PATH. rustflags go in the config because
/// RUSTFLAGS from the environment would replace them.
pub fn setup(ctx: &Ctx, attrs: &Attrs) -> io::Result<BTreeMap<String, String>> {
   let options: Options = ctx.options("cargo")?;
   let home = format!("{}/cargo-home", ctx.build);
   create_dir_all(&home)?;
   let rustc = tool("rustc")?;
   let mut env = BTreeMap::from([
      ("CARGO_HOME".to_owned(), home.clone()),
      (
         "CARGO_TARGET_DIR".to_owned(),
         format!("{}/target", ctx.build),
      ),
      ("RUSTC".to_owned(), rustc.to_string_lossy().into_owned()),
   ]);
   let version = Command::new(&rustc).arg("-vV").output()?;
   let host = String::from_utf8_lossy(&version.stdout)
      .lines()
      .find_map(|line| line.strip_prefix("host: "))
      .ok_or_else(|| io::Error::other("rustc -vV names no host"))?
      .to_owned();
   let platform = ctx.platform()?;
   let target = platform.rust_triple;
   env.insert("CARGO_BUILD_TARGET".to_owned(), target.clone());
   let spec = ctx.spec()?;
   syslibs::check(attrs, &ctx.project_dir(&options.root), spec.sys.as_deref())?;
   let sys = syslibs::active(attrs, "cargo", &ctx.deps)?.env;
   env.insert("PKG_CONFIG_ALLOW_CROSS".to_owned(), "1".to_owned());
   if !sys.is_empty() {
      let names: Vec<&str> = sys.keys().map(String::as_str).collect();
      note("sys-libs", &names.join(" "));
   }
   env.extend(sys);
   env.insert(
      "CLANG_PATH".to_owned(),
      on_path("cc").to_string_lossy().into_owned(),
   );
   let mut rustflags = vec![
      format!("--remap-path-prefix={}=/src", ctx.src),
      format!("--remap-path-prefix={home}=/cargo"),
      format!(
         "--remap-path-prefix={}=/vendor",
         options.deps.as_deref().unwrap_or_default()
      ),
      "-Cforce-frame-pointers=yes".to_owned(),
   ];
   if platform.cross {
      rustflags.push(format!(
         "--sysroot={}",
         cross_sysroot(ctx, attrs, &rustc, &target)?
      ));
   }
   let vendor = match options.deps.as_deref() {
      Some(deps) if !options.crate_patches.is_empty() => {
         Some(patched_vendor(ctx, deps, &options.crate_patches)?)
      },
      Some(deps) => Some(deps.to_owned()),
      None => None,
   };
   let mut config = String::new();
   if let Some(directory) = vendor {
      writeln!(
         config,
         "[source.crates-io]\nreplace-with = \"vendored\"\n\n[source.vendored]\ndirectory = {}\n",
         quoted(&directory)
      )
      .map_err(io::Error::other)?;
   }
   writeln!(
      config,
      "[net]\noffline = true\n\n[build]\njobs = {}\n",
      ctx.njobs
   )
   .map_err(io::Error::other)?;
   // host first, natively it is the target and cc wins
   let build_linker = var("CC_FOR_BUILD").unwrap_or_default();
   let mut linkers = vec![(host.as_str(), build_linker.as_str())];
   match linkers.iter_mut().find(|entry| entry.0 == target) {
      Some(entry) => entry.1 = "cc",
      None => linkers.push((target.as_str(), "cc")),
   }
   for (triple, linker) in linkers {
      writeln!(
         config,
         "[target.{}]\nlinker = {}\nrustflags = {}\n",
         quoted(triple),
         quoted(linker),
         list(&rustflags)
      )
      .map_err(io::Error::other)?;
   }
   write_file(
      Path::new(&format!("{home}/config.toml")),
      config.as_bytes(),
      None,
   )?;
   Ok(env)
}

// the toolchain carries std for the build machine only, the target's is
// <toolchain>-std.
fn cross_sysroot(ctx: &Ctx, attrs: &Attrs, rustc: &Path, target: &str) -> io::Result<String> {
   let sysroot = format!("{}/rust-sysroot", ctx.build);
   let toolchain = rustc
      .parent()
      .and_then(Path::parent)
      .ok_or_else(|| io::Error::other("rustc has no installation root"))?;
   let wanted = format!("{}-std", exports_of(toolchain)?.name);
   let std = attrs
      .build_dependencies
      .iter()
      .find(|root| exports_of(Path::new(root)).is_ok_and(|found| found.name == wanted))
      .ok_or_else(|| {
         io::Error::other(format!("buildPkgs.{wanted} must be in buildDependencies"))
      })?;
   create_dir_all(format!("{sysroot}/lib/rustlib"))?;
   for entry in read_dir(toolchain.join("lib/rustlib"))? {
      let found = entry?;
      symlink(
         found.path(),
         format!(
            "{sysroot}/lib/rustlib/{}",
            found.file_name().to_string_lossy()
         ),
      )?;
   }
   symlink(
      format!("{std}/lib/rustlib/{target}"),
      format!("{sysroot}/lib/rustlib/{target}"),
   )?;
   Ok(sysroot)
}

/// the vendor dir with the named crates copied out and patched, the rest
/// symlinked.
fn patched_vendor(
   ctx: &Ctx,
   deps: &str,
   patches: &BTreeMap<String, Vec<String>>,
) -> io::Result<String> {
   let dir = format!("{}/vendor", ctx.build);
   create_dir_all(&dir)?;
   let mut names: Vec<String> = read_dir(deps)?
      .map(|entry| entry.map(|found| found.file_name().to_string_lossy().into_owned()))
      .collect::<io::Result<_>>()?;
   names.sort();
   for name in &names {
      symlink(format!("{deps}/{name}"), format!("{dir}/{name}"))?;
   }
   for (crate_name, files) in patches {
      let hits: Vec<&String> = names
         .iter()
         .filter(|name| crate_of(name) == Some(crate_name.as_str()))
         .collect();
      if hits.is_empty() {
         return Err(io::Error::other(format!(
            "cargo.cratePatches: no vendored crate {crate_name}"
         )));
      }
      for name in hits {
         let placed = format!("{dir}/{name}");
         remove_file(&placed)?;
         succeed(
            Command::new("cp")
               .arg("-r")
               .arg(format!("{deps}/{name}"))
               .arg(&placed),
            "cp",
         )?;
         succeed(
            Command::new("chmod").arg("-R").arg("u+w").arg(&placed),
            "chmod",
         )?;
         for file in files {
            note("patch", &format!("{name}: {file}"));
            succeed(
               Command::new("patch")
                  .arg("-d")
                  .arg(&placed)
                  .args(["-p1", "-F0", "-i"])
                  .arg(file),
               "patch",
            )?;
         }
      }
   }
   Ok(dir)
}

/// `name-1.2.3` is the crate `name`, the version starts at the last dash
/// followed by a digit.
fn crate_of(directory: &str) -> Option<&str> {
   let dash = directory
      .rmatch_indices('-')
      .find(|hit| directory[hit.0 + 1..].starts_with(|found: char| found.is_ascii_digit()))?
      .0;
   Some(&directory[..dash])
}

pub fn phase(ctx: &Ctx, verb: &str) -> io::Result<()> {
   let options: Options = ctx.options("cargo")?;
   match verb {
      "build" => {
         let mut args = vec![
            "build".to_owned(),
            "--release".to_owned(),
            "--offline".to_owned(),
         ];
         args.extend(arguments(&options));
         run("cargo", &args, &[])
      },
      "test" => {
         let mut args = vec![
            "test".to_owned(),
            "--release".to_owned(),
            "--offline".to_owned(),
         ];
         args.extend(arguments(&options));
         args.push("--".to_owned());
         args.push("--test-threads".to_owned());
         args.push(ctx.test_jobs()?.to_string());
         for skipped in &options.skip_tests {
            args.push("--skip".to_owned());
            args.push(skipped.clone());
         }
         run("cargo", &args, &[])
      },
      "install" => install(ctx),
      _ => Err(unknown("cargo", verb)),
   }
}

/// with `CARGO_BUILD_TARGET` set cargo always builds into target/<triple>/. Its
/// own files there are lib*.rlib/.so/.d and .cargo-lock, the rest are the
/// [[bin]] targets.
fn install(ctx: &Ctx) -> io::Result<()> {
   let release = format!(
      "{}/{}/release",
      var("CARGO_TARGET_DIR").unwrap_or_default(),
      var("CARGO_BUILD_TARGET").unwrap_or_default()
   );
   let spec = ctx.spec()?;
   let mut names: Vec<String> = Vec::new();
   for entry in read_dir(&release)? {
      let found = entry?;
      let name = found.file_name().to_string_lossy().into_owned();
      let extension = Path::new(&name)
         .extension()
         .map(|ext| ext.to_string_lossy().to_ascii_lowercase());
      if !symlink_metadata(found.path())?.is_file()
         || name.starts_with("lib")
         || name.starts_with('.')
         || extension.as_deref() == Some("d")
         || extension.as_deref() == Some("rlib")
         || spec
            .bin
            .as_ref()
            .is_some_and(|listed| !listed.contains(&name))
      {
         continue;
      }
      names.push(name);
   }
   names.sort();
   let bin = format!("{}/bin", ctx.out);
   create_dir_all(&bin)?;
   for name in names {
      copy(format!("{release}/{name}"), format!("{bin}/{name}"))?;
   }
   Ok(())
}
