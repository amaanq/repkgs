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

use mummer::{
   files::{
      utf8,
      write_file,
   },
   process::Checked as _,
   store::on_path,
   syslibs::{
      CargoBuild,
      Ecosystem,
      feature_flags,
   },
   tree::note,
};

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
         file_names,
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
   fn module(&self) -> &'static str {
      module_path!()
   }

   fn name(&self) -> &'static str {
      "cargo"
   }

   fn options(&self) -> io::Result<Vec<Field>> {
      Options::schema(self.name())
   }

   fn setup(&self, ctx: &Ctx, attrs: &Attrs, _socket: &Path) -> io::Result<Env> {
      let options = ctx.options::<Options>(self.name())?;
      let vendor = match options.deps.as_deref() {
         Some(deps) if !options.crate_patches.is_empty() => {
            Some(Self::patched_vendor(ctx, deps, &options.crate_patches)?)
         },
         Some(deps) => Some(deps.to_owned()),
         None => None,
      };
      let home = format!("{}/cargo-home", ctx.build.display());
      let mut env = Env::from(Self::configure(ctx, attrs, vendor.as_deref(), &home)?);
      env.unset("RUSTFLAGS");
      Ok(env)
   }

   fn phase(&self, ctx: &Ctx, _attrs: &Attrs, verb: &str, _socket: &Path) -> io::Result<()> {
      let options = ctx.options::<Options>(self.name())?;
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
         _ => Err(unknown(self.name(), verb)),
      }
   }
}

impl Cargo {
   fn build(options: &Options, home: Option<PathBuf>) -> CargoBuild {
      CargoBuild {
         features: options.features.clone(),
         no_default: options.no_default_features,
         without: options.without_features.clone(),
         home,
      }
   }

   fn arguments(ctx: &Ctx, options: &Options) -> io::Result<Vec<String>> {
      let home = ctx.build.join("cargo-home");
      let mut args = feature_flags(
         &ctx.project_dir(&options.root),
         &Self::build(options, Some(home)),
      )?;
      args.extend(options.flags.iter().cloned());
      Ok(args)
   }

   fn quoted(value: &str) -> String {
      format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
   }

   fn list(values: &[String]) -> String {
      let items = values
         .iter()
         .map(|value| Self::quoted(value))
         .collect::<Vec<String>>();
      format!("[{}]", items.join(", "))
   }

   /// pkg-config, the -sys crates' usual probe, refuses to answer under
   /// --target without `ALLOW_CROSS`. bindgen asks a clang executable for
   /// its include dirs, ours and not a bare one on PATH. rustflags go in the
   /// config because RUSTFLAGS from the environment would replace them, the
   /// linker in the environment too since a project's own .cargo/config.toml
   /// outranks `CARGO_HOME`'s.
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
            format!("{}/target", ctx.build.display()),
         ),
         ("RUSTC".to_owned(), utf8(&rustc)?.to_owned()),
      ]);
      let version = Command::new(&rustc).arg("-vV").checked_output()?;
      let host = String::from_utf8_lossy(&version)
         .lines()
         .find_map(|line| line.strip_prefix("host: "))
         .ok_or_else(|| io::Error::other("rustc -vV names no host"))?
         .to_owned();
      let platform = ctx.platform()?;
      let target = platform.rust_triple;
      env.insert("CARGO_BUILD_TARGET".to_owned(), target.clone());
      let sys = syslibs::active(attrs, Ecosystem::Cargo, &ctx.deps).env;
      env.insert("PKG_CONFIG_ALLOW_CROSS".to_owned(), "1".to_owned());
      if !sys.is_empty() {
         let names = sys.keys().map(String::as_str).collect::<Vec<&str>>();
         note("sys-libs", &names.join(" "));
      }
      env.extend(sys);
      env.insert("CLANG_PATH".to_owned(), utf8(&on_path("cc"))?.to_owned());
      if let Some(stdlib) = Self::cxx_stdlib()? {
         env.insert("CXXSTDLIB".to_owned(), stdlib);
      }
      let mut rustflags = vec![
         format!("--remap-path-prefix={}=/src", ctx.src.display()),
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
         if !linker.is_empty() {
            env.insert(
               format!(
                  "CARGO_TARGET_{}_LINKER",
                  triple.to_uppercase().replace('-', "_")
               ),
               linker.to_owned(),
            );
         }
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

   /// cc-rs links libstdc++ on Linux unless `CXXSTDLIB` says otherwise.
   fn cxx_stdlib() -> io::Result<Option<String>> {
      let output = Command::new("c++")
         .args(["-###", "-x", "c++", "/dev/null"])
         .output()?;
      let jobs = String::from_utf8_lossy(&output.stderr);
      Ok(jobs.split_whitespace().find_map(|word| {
         let library = word.trim_matches('"').strip_prefix("-l")?;
         ["c++", "stdc++"]
            .contains(&library)
            .then(|| library.to_owned())
      }))
   }

   // the toolchain carries std for the build machine only, the target's is
   // <toolchain>-std.
   fn cross_sysroot(ctx: &Ctx, attrs: &Attrs, rustc: &Path, target: &str) -> io::Result<String> {
      let sysroot = format!("{}/rust-sysroot", ctx.build.display());

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
               utf8(Path::new(&found.file_name()))?
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
      let dir = format!("{}/vendor", ctx.build.display());
      create_dir_all(&dir)?;
      let names = file_names(Path::new(deps))?;
      for name in &names {
         symlink(format!("{deps}/{name}"), format!("{dir}/{name}"))?;
      }
      for (crate_name, files) in patches {
         let hits = names
            .iter()
            .filter(|name| Self::crate_of(name) == Some(crate_name.as_str()))
            .collect::<Vec<&String>>();
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
            )?;
            succeed(Command::new("chmod").arg("-R").arg("u+w").arg(&placed))?;
            for file in files {
               note("patch", &format!("{name}: {}", file.display()));
               succeed(
                  Command::new("patch")
                     .arg("-d")
                     .arg(&placed)
                     .args(["-p1", "-F0", "-i"])
                     .arg(file),
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
      let mut names = Vec::<String>::new();
      for entry in read_dir(&release)? {
         let found = entry?;
         let name = utf8(Path::new(&found.file_name()))?.to_owned();
         let binary = name.strip_suffix(exe.as_str()).unwrap_or(&name);
         let extension = Path::new(&name)
            .extension()
            .and_then(|ext| ext.to_str())
            .map(str::to_ascii_lowercase);
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
      let bin = ctx.out.join("bin");
      create_dir_all(&bin)?;
      for name in names {
         copy(format!("{release}/{name}"), bin.join(&name))?;
      }
      Ok(())
   }
}
