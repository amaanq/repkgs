use std::{
   collections::BTreeMap,
   env::var,
   fmt::Write as _,
   fs::{
      canonicalize,
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

use mummer::{
   files::write_file,
   store::on_path,
   tree::note,
};
use serde::Deserialize;

use crate::{
   attrs::{
      Attrs,
      Ctx,
   },
   exports::exports_of,
   prepare::succeed,
   schema::{
      Field,
      PackagePath,
   },
   system::{
      command::{
         Env,
         run,
         strs,
         tool,
         unknown,
      },
      dispatch::System,
      syslibs,
   },
};

options! {
   Options {
      root ("root"): String = ".".to_owned()
         => "directory below the source the project lives in (monorepos, build/cmake)",
      features ("features"): Vec<String> = Vec::new()
         => "--features",
      no_default_features ("noDefaultFeatures"): bool = false
         => "--no-default-features",
      without_features ("withoutFeatures"): Vec<String> = Vec::new()
         => "default features to leave out, the rest of the package's default set stays on",
      flags ("flags"): Vec<String> = Vec::new()
         => "extra arguments for cargo build and cargo test",
      skip_tests ("skipTests"): Vec<String> = Vec::new()
         => "cargo test --skip filters (substring of the test path)",
      deps ("deps"): Option<PackagePath> = None
         => "the locked dependencies, fetched (fetch.cargoVendor), by default from the package's own lock file",
      crate_patches ("cratePatches"): BTreeMap<String, Vec<PathBuf>> = BTreeMap::new()
         => "crate name -> patches applied to its vendored copy (-p1 inside the crate)",
   }
}

pub struct Cargo;

impl System for Cargo {
   fn name(&self) -> &'static str {
      "cargo"
   }

   fn options(&self) -> io::Result<Vec<Field>> {
      Options::schema(self.name())
   }

   fn setup(&self, ctx: &Ctx, attrs: &Attrs, _socket: &Path) -> io::Result<Env> {
      let options: Options = ctx.options("cargo")?;
      let vendor = match options.deps.as_deref() {
         Some(deps) if !options.crate_patches.is_empty() => {
            Some(Self::patched_vendor(ctx, deps, &options.crate_patches)?)
         },
         Some(deps) => Some(deps.to_owned()),
         None => None,
      };
      syslibs::check(
         attrs,
         &ctx.project_dir(&options.root),
         ctx.spec()?.sys.as_deref(),
      )?;
      let mut env: Env = Self::configure(
         ctx,
         attrs,
         vendor.as_deref(),
         &format!("{}/cargo-home", ctx.build),
      )?
      .into();
      env.unset("RUSTFLAGS");
      Ok(env)
   }

   fn phase(&self, ctx: &Ctx, _attrs: &Attrs, verb: &str, _socket: &Path) -> io::Result<()> {
      let options: Options = ctx.options("cargo")?;
      let extra = Self::arguments(ctx, &options)?;
      match verb {
         "build" => {
            let mut args = vec!["build", "--release", "--offline"];
            args.extend(strs(&extra));
            run("cargo", &args, &[])
         },
         "test" => {
            let threads = ctx.test_jobs()?.to_string();
            let mut args = vec!["test", "--release", "--offline"];
            args.extend(strs(&extra));
            args.extend(["--", "--test-threads", &threads]);
            for skipped in &options.skip_tests {
               args.extend(["--skip", skipped]);
            }
            run("cargo", &args, &[])
         },
         "install" => Self::install(ctx),
         _ => Err(unknown("cargo", verb)),
      }
   }
}

impl Cargo {
   fn arguments(ctx: &Ctx, options: &Options) -> io::Result<Vec<String>> {
      let mut args = Vec::new();
      let mut features = options.features.clone();
      if options.no_default_features || !options.without_features.is_empty() {
         args.push("--no-default-features".to_owned());
      }
      if !options.without_features.is_empty() {
         features.extend(Self::kept_defaults(
            &ctx.project_dir(&options.root),
            &options.without_features,
         )?);
      }
      if !features.is_empty() {
         args.push(format!("--features={}", features.join(",")));
      }
      args.extend(options.flags.iter().cloned());
      Ok(args)
   }

   /// The package's own default features minus `without`, as cargo reads
   /// them, so a feature a new release adds to the default set stays on.
   fn kept_defaults(project: &Path, without: &[String]) -> io::Result<Vec<String>> {
      #[derive(Deserialize)]
      struct Metadata {
         packages: Vec<Package>,
      }
      #[derive(Deserialize)]
      struct Package {
         manifest_path: PathBuf,
         features:      BTreeMap<String, Vec<String>>,
      }

      let output = Command::new("cargo")
         .args([
            "metadata",
            "--no-deps",
            "--offline",
            "--format-version",
            "1",
         ])
         .current_dir(project)
         .output()?;
      if !output.status.success() {
         return Err(io::Error::other(format!(
            "cargo metadata failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
         )));
      }
      let manifest = canonicalize(project.join("Cargo.toml"))?;
      let package = serde_json::from_slice::<Metadata>(&output.stdout)?
         .packages
         .into_iter()
         .find(|package| package.manifest_path == manifest)
         .ok_or_else(|| {
            io::Error::other(format!("cargo metadata lists no {}", manifest.display()))
         })?;
      let defaults = package.features.get("default").cloned().unwrap_or_default();
      if let Some(missing) = without.iter().find(|name| !defaults.contains(name)) {
         return Err(io::Error::other(format!(
            "withoutFeatures names {missing}, which is not among the default features {}",
            defaults.join(", ")
         )));
      }
      Ok(defaults
         .into_iter()
         .filter(|name| !without.contains(name))
         .collect())
   }

   fn quoted(value: &str) -> String {
      format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
   }

   fn list(values: &[String]) -> String {
      let items: Vec<String> = values.iter().map(|value| Self::quoted(value)).collect();
      format!("[{}]", items.join(", "))
   }

   /// pkg-config, the -sys crates' usual probe, refuses to answer under
   /// --target without `ALLOW_CROSS`. bindgen asks a clang executable for
   /// its include dirs, ours and not a bare one on PATH. rustflags go in the
   /// config because RUSTFLAGS from the environment would replace them.
   pub fn configure(
      ctx: &Ctx,
      attrs: &Attrs,
      vendor: Option<&str>,
      home: &str,
   ) -> io::Result<BTreeMap<String, String>> {
      create_dir_all(home)?;
      let rustc = tool("rustc")?;
      let mut env = BTreeMap::from([
         ("CARGO_HOME".to_owned(), home.to_owned()),
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
      ];
      if let Some(deps) = vendor {
         rustflags.push(format!("--remap-path-prefix={deps}=/vendor"));
      }
      rustflags.push("-Cforce-frame-pointers=yes".to_owned());
      if platform.cross {
         rustflags.push(format!(
            "--sysroot={}",
            Self::cross_sysroot(ctx, attrs, &rustc, &target)?
         ));
      }
      let mut config = String::new();
      if let Some(directory) = vendor {
         writeln!(
            config,
            "[source.crates-io]\nreplace-with = \"vendored\"\n\n[source.vendored]\ndirectory = \
             {}\n",
            Self::quoted(directory)
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
            Self::quoted(triple),
            Self::quoted(linker),
            Self::list(&rustflags)
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

      if Path::new(&sysroot).is_dir() {
         return Ok(sysroot);
      }

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
      patches: &BTreeMap<String, Vec<PathBuf>>,
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
            .filter(|name| Self::crate_of(name) == Some(crate_name.as_str()))
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
               note("patch", &format!("{name}: {}", file.display()));
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

   /// with `CARGO_BUILD_TARGET` set cargo always builds into target/<triple>/.
   /// Its own files there are lib*.rlib/.so/.d and .cargo-lock, the rest are
   /// the [[bin]] targets.
   fn install(ctx: &Ctx) -> io::Result<()> {
      let release = format!(
         "{}/{}/release",
         var("CARGO_TARGET_DIR").unwrap_or_default(),
         var("CARGO_BUILD_TARGET").unwrap_or_default()
      );
      let spec = ctx.spec()?;
      let exe = ctx.platform()?.ext.exe;
      let mut names: Vec<String> = Vec::new();
      for entry in read_dir(&release)? {
         let found = entry?;
         let name = found.file_name().to_string_lossy().into_owned();
         let binary = name.strip_suffix(exe.as_str()).unwrap_or(&name);
         let extension = Path::new(&name)
            .extension()
            .map(|ext| ext.to_string_lossy().to_ascii_lowercase());
         if !symlink_metadata(found.path())?.is_file()
            || name.starts_with("lib")
            || name.starts_with('.')
            || extension.as_deref() == Some("d")
            || extension.as_deref() == Some("rlib")
            || extension.as_deref() == Some("pdb")
            || spec
               .bin
               .as_ref()
               .is_some_and(|listed| !listed.iter().any(|candidate| candidate == binary))
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
}
