use std::{
   collections::{
      BTreeMap,
      BTreeSet,
   },
   convert::Infallible,
   env::var,
   ffi::{
      OsStr,
      OsString,
   },
   fmt::{
      Display,
      Formatter,
      Result as FormatResult,
      Write as _,
   },
   fs::{
      copy,
      create_dir_all,
      read,
      read_dir,
      remove_file,
      symlink_metadata,
      write,
   },
   io,
   os::unix::fs::symlink,
   path::{
      Path,
      PathBuf,
   },
   process::{
      Command,
      Stdio,
   },
   str::FromStr,
};

use mummer::{
   files::{
      utf8,
      write_file,
   },
   process::{
      Checked as _,
      loose,
      strict,
   },
   store::on_path,
   syslibs::{
      CargoBuild,
      Ecosystem,
      feature_flags,
   },
   tree::note,
};
use serde::{
   Deserialize,
   Serialize,
};

use crate::{
   attrs::{
      Attrs,
      Ctx,
      build_top,
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
   test_cache,
   trace::Tests,
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

   fn tests(&self) -> Option<&'static dyn Tests> {
      Some(&Self)
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
         "test-prepare" => Self::prepare_tests(&extra),
         "test" => {
            let threads = ctx.test_jobs()?.to_string();
            let mut harness = vec!["--".to_owned(), "--test-threads".to_owned(), threads];
            for skipped in &options.skip_tests {
               harness.extend(["--skip".to_owned(), skipped.clone()]);
            }
            let Some(selected) = test_cache::selected::<CargoTest>()? else {
               return Self::test(&extra, &[], &harness);
            };
            let noted = read(Path::new(&build_top()?).join(TARGETS))?;
            let targets = serde_json::from_slice::<BTreeMap<String, Target>>(&noted)?;
            let mut chosen = BTreeSet::new();
            let mut doc = false;
            for test in selected {
               match test {
                  CargoTest::Doc(_) => doc = true,
                  CargoTest::Binary(file) => {
                     let target = targets.get(&file).ok_or_else(|| {
                        io::Error::other(format!("{file} is no test binary cargo built"))
                     })?;
                     chosen.insert(target.selector());
                  },
               }
            }
            if !chosen.is_empty() {
               Self::test(
                  &extra,
                  &chosen.into_iter().flatten().collect::<Vec<_>>(),
                  &harness,
               )?;
            }
            if doc {
               Self::test(&extra, &["--doc".to_owned()], &harness)?;
            }
            Ok(())
         },
         "install" => Self::install(ctx),
         _ => Err(unknown(self.name(), verb)),
      }
   }
}

/// Where `test-prepare` leaves the test binaries' targets, by file name.
const TARGETS: &str = ".bigtop-cargo-tests.json";

/// What `cargo test` runs as one test, a test binary by its file name or a
/// crate's doctests.
enum CargoTest {
   Binary(String),
   Doc(String),
}

impl Display for CargoTest {
   fn fmt(&self, f: &mut Formatter<'_>) -> FormatResult {
      match *self {
         Self::Binary(ref file) => f.write_str(file),
         Self::Doc(ref name) => write!(f, "doc:{name}"),
      }
   }
}

impl FromStr for CargoTest {
   type Err = Infallible;

   fn from_str(text: &str) -> Result<Self, Self::Err> {
      Ok(text.strip_prefix("doc:").map_or_else(
         || Self::Binary(text.to_owned()),
         |name| Self::Doc(name.to_owned()),
      ))
   }
}

/// A target a test binary was built from, as `cargo test` selects it.
#[derive(Deserialize, Serialize)]
enum Target {
   /// Every member library's unit tests, since naming one library takes
   /// `-p`, which resolves features apart from the full run.
   Lib,
   Bin(String),
   Test(String),
   Bench(String),
   Example(String),
}

impl Target {
   fn selector(&self) -> Vec<String> {
      match *self {
         Self::Lib => vec!["--lib".to_owned()],
         Self::Bin(ref name) => vec!["--bin".to_owned(), name.clone()],
         Self::Test(ref name) => vec!["--test".to_owned(), name.clone()],
         Self::Bench(ref name) => vec!["--bench".to_owned(), name.clone()],
         Self::Example(ref name) => vec!["--example".to_owned(), name.clone()],
      }
   }
}

#[derive(Deserialize)]
struct Built {
   kind: Vec<String>,
   name: String,
}

#[derive(Deserialize)]
#[serde(tag = "reason", rename_all = "kebab-case")]
enum Message {
   CompilerArtifact {
      target:     Built,
      executable: Option<PathBuf>,
   },
   #[serde(other)]
   Other,
}

/// Test binaries are named for their target with a hash cargo keeps across
/// source edits, and `rustdoc --test` runs a crate's doctests.
impl Tests for Cargo {
   fn unit(&self, exe: &Path, args: &[OsString], _cwd: &Path) -> Option<String> {
      let test = if exe.parent()?.file_name()? == "deps" {
         CargoTest::Binary(exe.file_name()?.to_str()?.to_owned())
      } else if exe.file_name()? == "rustdoc" && args.iter().any(|arg| arg == "--test") {
         let at = args.iter().position(|arg| arg == "--crate-name")?;
         CargoTest::Doc(args.get(at + 1)?.to_str()?.to_owned())
      } else {
         return None;
      };
      Some(test.to_string())
   }

   fn prepares(&self) -> bool {
      true
   }

   /// Bookkeeping every cargo invocation rewrites.
   fn state(&self, path: &Path) -> bool {
      let fingerprint = path
         .components()
         .any(|component| component.as_os_str() == ".fingerprint");
      fingerprint
         || path
            .file_name()
            .and_then(OsStr::to_str)
            .is_some_and(|name| {
               name.starts_with(".global-cache")
                  || name.starts_with(".package-cache")
                  || name == ".rustc_info.json"
            })
   }
}

impl Cargo {
   fn test(extra: &[String], selectors: &[String], harness: &[String]) -> io::Result<()> {
      let mut args = vec!["test", "--release", "--offline"];
      args.extend(Self::scope(extra));
      args.extend(strs(extra));
      args.extend(strs(selectors));
      args.extend(strs(harness));
      run("cargo", &args, &[])
   }

   /// Builds the test binaries and notes each one's target, so the traced
   /// run compiles nothing and can be limited to some of them.
   fn prepare_tests(extra: &[String]) -> io::Result<()> {
      let mut args = vec![
         "test",
         "--release",
         "--offline",
         "--no-run",
         "--message-format=json-render-diagnostics",
      ];
      args.extend(Self::scope(extra));
      args.extend(strs(extra));
      let output = Command::new("cargo")
         .args(&args)
         .stderr(Stdio::inherit())
         .output()?;
      if !output.status.success() {
         return Err(io::Error::other(format!(
            "cargo test --no-run failed, {}",
            output.status
         )));
      }
      let mut targets = BTreeMap::new();
      for line in output.stdout.split(|byte| *byte == b'\n') {
         let Ok(Message::CompilerArtifact { target, executable }) =
            serde_json::from_slice::<Message>(line)
         else {
            continue;
         };
         let kind = target.kind.first().map_or("", String::as_str);
         let Some(file) = executable
            .filter(|path| path.parent().and_then(Path::file_name) == Some(OsStr::new("deps")))
            .and_then(|path| path.file_name()?.to_str().map(str::to_owned))
         else {
            continue;
         };
         let built = match kind {
            "bin" => Target::Bin(target.name),
            "test" => Target::Test(target.name),
            "bench" => Target::Bench(target.name),
            "example" => Target::Example(target.name),
            _ => Target::Lib,
         };
         targets.insert(file, built);
      }
      write(
         Path::new(&build_top()?).join(TARGETS),
         serde_json::to_vec(&targets)?,
      )
   }

   /// Every member unless the flags pick packages, since `--workspace`
   /// overrides `-p`.
   fn scope(extra: &[String]) -> Option<&'static str> {
      let picks = extra
         .iter()
         .any(|arg| arg == "--package" || arg.starts_with("--package=") || arg.starts_with("-p"));
      (!picks).then_some("--workspace")
   }

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
      let host = strict(&version, "rustc -vV")?
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
      let jobs = loose(&output.stderr);
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
