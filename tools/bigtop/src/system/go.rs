use std::{
   collections::{
      BTreeMap,
      BTreeSet,
   },
   ffi::OsString,
   fs::{
      copy,
      create_dir_all,
      read,
      write,
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
      Output,
   },
   sync::{
      Mutex,
      OnceLock,
   },
   thread::scope,
};

use mummer::{
   process::Checked as _,
   syslibs::Ecosystem,
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
      tags ("tags"): Vec<String> = Vec::new()
         => "-tags",
      ldflags ("ldflags"): Vec<String> = Vec::new()
         => "-ldflags words (-X main.version=\u{2026})",
      packages ("packages"): Vec<String> = vec!["./...".to_owned()]
         => "packages to build",
      test_packages ("testPackages"): Option<Vec<String>> = None
         => "packages to test (null uses `packages`)",
      skip_tests ("skipTests"): Vec<String> = Vec::new()
         => "go test -skip regexes",
      deps ("deps"): Option<PackagePath> = None
         => "GOPROXY tree (fetch.goModules, by default from go.sum), or null to build from the source's vendor/",
      cgo ("cgo"): bool = true
         => "CGO_ENABLED and external linking",
      flags ("flags"): Vec<String> = Vec::new()
         => "extra arguments for go build and go test",
      module_patches ("modulePatches"): BTreeMap<String, Vec<PathBuf>> = BTreeMap::new()
         => "module path -> patches applied to a copy of it (-p1 inside the module), swapped in with a go.mod replace",
   }
}

#[derive(Deserialize)]
struct Download {
   #[serde(rename = "Dir")]
   dir: String,
}

pub struct Go;

impl System for Go {
   fn module(&self) -> &'static str {
      module_path!()
   }

   fn name(&self) -> &'static str {
      "go"
   }

   fn options(&self) -> io::Result<Vec<Field>> {
      Options::schema(self.name())
   }

   fn tests(&self) -> Option<&'static dyn Tests> {
      Some(&Self)
   }

   /// compile, asm and link take a host-wide mummerd slot like cc and rustc do.
   fn setup(&self, ctx: &Ctx, _attrs: &Attrs, _socket: &Path) -> io::Result<Env> {
      let options = ctx.options::<Options>(self.name())?;
      let platform = ctx.platform()?;
      let (mode, proxy) = options.deps.as_deref().map_or_else(
         || ("vendor", "off".to_owned()),
         |deps| ("mod", format!("file://{deps}")),
      );
      let vars = [
         ("GOCACHE", format!("{}/go-cache", ctx.build.display())),
         ("GOPATH", format!("{}/go", ctx.build.display())),
         ("GOSUMDB", "off".to_owned()),
         ("GOTOOLCHAIN", "local".to_owned()),
         (
            "GOFLAGS",
            format!(
               "-mod={mode} -trimpath -buildvcs=false '-toolexec={} slot'",
               tool("mummer")?.display()
            ),
         ),
         ("GOPROXY", proxy),
         (
            "CGO_ENABLED",
            if options.cgo { "1" } else { "0" }.to_owned(),
         ),
         ("GOOS", platform.os_names.go),
         ("GOARCH", platform.names.go),
      ];
      if !options.module_patches.is_empty() {
         if options.deps.is_none() {
            return Err(io::Error::other(
               "go.modulePatches needs go.deps, a vendor/ tree takes no replace",
            ));
         }
         Self::patch_modules(ctx, &vars, &options.module_patches)?;
      }
      Ok(vars.into())
   }

   fn phase(&self, ctx: &Ctx, attrs: &Attrs, verb: &str, _socket: &Path) -> io::Result<()> {
      let options = ctx.options::<Options>(self.name())?;
      match verb {
         "build" => {
            let bin = format!("{}/bin/", ctx.build.display());
            create_dir_all(&bin)?;
            let jobs = format!("-p={}", ctx.njobs);
            let common = Self::common_args(ctx, attrs, &options)?;
            let mut args = vec!["build", &jobs, "-o", &bin];
            args.extend(strs(&common));
            args.extend(strs(&options.packages));
            run("go", &args, &[])
         },
         "test-prepare" => Self::prepare_tests(ctx, attrs, &options),
         "test" => Self::test(ctx, &options),
         "install" => Self::install(ctx),
         _ => Err(unknown(self.name(), verb)),
      }
   }
}

impl Go {
   /// Lists the packages with tests and compiles their test binaries ahead of
   /// the traced run, in batches whose package names differ, since `go test
   /// -c -o dir/` names each binary after its package.
   fn prepare_tests(ctx: &Ctx, attrs: &Attrs, options: &Options) -> io::Result<()> {
      let common = Self::common_args(ctx, attrs, options)?;
      let packages = options.test_packages.as_ref().unwrap_or(&options.packages);
      let mut listing = vec!["list", "-json=ImportPath,Name,Dir,TestGoFiles,XTestGoFiles"];
      listing.extend(strs(&common));
      listing.extend(strs(packages));
      let listed = Command::new("go").args(&listing).checked_output()?;
      let mut batches = Vec::<BTreeMap<String, Listed>>::new();
      for found in serde_json::Deserializer::from_slice(&listed).into_iter::<Listed>() {
         let package = found?;
         if package.test_go_files.is_empty() && package.x_test_go_files.is_empty() {
            continue;
         }
         match batches
            .iter_mut()
            .find(|batch| !batch.contains_key(&package.name))
         {
            Some(batch) => {
               batch.insert(package.name.clone(), package);
            },
            None => batches.push(BTreeMap::from([(package.name.clone(), package)])),
         }
      }
      let root = ctx.build.join("go-tests");
      let mut binaries = BTreeMap::new();
      for (index, batch) in batches.into_iter().enumerate() {
         let directory = root.join(index.to_string());
         create_dir_all(&directory)?;
         let output = format!("{}/", directory.display());
         let mut compile = vec!["test", "-c", "-vet=off", "-o", &output];
         compile.extend(strs(&common));
         compile.extend(batch.values().map(|package| package.import_path.as_str()));
         run("go", &compile, &[])?;
         for (name, package) in batch {
            let binary = directory.join(format!("{name}.test"));
            if binary.exists() {
               binaries.insert(binary, Binary {
                  package: package.import_path,
                  dir:     package.dir,
               });
            }
         }
      }
      write(
         Path::new(&build_top()?).join(BINARIES),
         serde_json::to_vec(&binaries)?,
      )
   }

   /// Runs the prepared test binaries as `go test` would, each in its
   /// package's directory and `-p` of them at once, only the selected
   /// packages' when the test cache picked some.
   fn test(ctx: &Ctx, options: &Options) -> io::Result<()> {
      let jobs = ctx.test_jobs()?;
      let binaries = serde_json::from_slice::<BTreeMap<PathBuf, Binary>>(&read(
         Path::new(&build_top()?).join(BINARIES),
      )?)?;
      let chosen = test_cache::selected::<String>()?.map(BTreeSet::from_iter);
      let mut args = vec![
         "-test.paniconexit0".to_owned(),
         "-test.timeout=10m0s".to_owned(),
         format!("-test.parallel={jobs}"),
      ];
      if !options.skip_tests.is_empty() {
         args.push(format!("-test.skip={}", options.skip_tests.join("|")));
      }
      let pending = Mutex::new(binaries.iter().filter(|entry| {
         chosen
            .as_ref()
            .is_none_or(|names| names.contains(&entry.1.package))
      }));
      let failed = Mutex::new(Vec::new());
      scope(|threads| {
         for _worker in 0..jobs {
            threads.spawn(|| {
               loop {
                  let Some((binary, entry)) = pending.lock().ok().and_then(|mut next| next.next())
                  else {
                     return;
                  };
                  let ran = Command::new(binary)
                     .args(&args)
                     .current_dir(&entry.dir)
                     .output();
                  let passed = ran.as_ref().is_ok_and(|output| output.status.success());
                  Self::report(&entry.package, &ran);
                  if !passed && let Ok(mut list) = failed.lock() {
                     list.push(entry.package.clone());
                  }
               }
            });
         }
      });
      let failures = failed
         .into_inner()
         .map_err(|_poisoned| io::Error::other("a test runner panicked"))?;
      if failures.is_empty() {
         return Ok(());
      }
      Err(io::Error::other(format!(
         "go tests failed in {}",
         failures.join(", ")
      )))
   }

   /// A package's output in one piece, as `go test` prints it.
   fn report(package: &str, ran: &io::Result<Output>) {
      let mut out = io::stdout().lock();
      match *ran {
         Ok(ref output) => {
            drop(out.write_all(&output.stdout));
            drop(out.write_all(&output.stderr));
            let verdict = if output.status.success() {
               "ok"
            } else {
               "FAIL"
            };
            drop(writeln!(out, "{verdict}\t{package}"));
         },
         Err(ref error) => drop(writeln!(out, "FAIL\t{package}\t{error}")),
      }
   }

   /// each patched module is copied out of the module cache and replaced by
   /// directory, since go.sum would reject a patched copy served by the proxy.
   fn patch_modules(
      ctx: &Ctx,
      vars: &[(&str, String)],
      patches: &BTreeMap<String, Vec<PathBuf>>,
   ) -> io::Result<()> {
      let go = |args: &[&str]| -> io::Result<String> {
         let output = Command::new("go")
            .args(args)
            .envs(vars.iter().map(|&(name, ref value)| (name, value)))
            .checked_output()?;
         String::from_utf8(output).map_err(io::Error::other)
      };
      for (module, files) in patches {
         let listed = go(&["list", "-m", "-f", "{{.Version}}", module])?;
         let version = listed.trim();
         let download = serde_json::from_str::<Download>(&go(&[
            "mod",
            "download",
            "-json",
            &format!("{module}@{version}"),
         ])?)?;
         let placed = ctx
            .build
            .join("go-patched")
            .join(format!("{}@{version}", module.replace('/', "_")));
         create_dir_all(ctx.build.join("go-patched"))?;
         Command::new("cp")
            .arg("-r")
            .arg(&download.dir)
            .arg(&placed)
            .checked()?;
         Command::new("chmod")
            .arg("-R")
            .arg("u+w")
            .arg(&placed)
            .checked()?;
         for file in files {
            note("patch", &format!("{module}: {}", file.display()));
            Command::new("patch")
               .arg("-d")
               .arg(&placed)
               .args(["-p1", "-F0", "-i"])
               .arg(file)
               .checked()?;
         }
         let target = placed
            .to_str()
            .ok_or_else(|| io::Error::other("patched module path is not UTF-8"))?;
         go(&["mod", "edit", &format!("-replace={module}={target}")])?;
      }
      Ok(())
   }

   /// cgo builds link through cc so the RUNPATH and interp policy apply, and
   /// gobuildid gives finish a GNU build-id for the debug split.
   fn common_args(ctx: &Ctx, attrs: &Attrs, options: &Options) -> io::Result<Vec<String>> {
      let mut tags = options.tags.clone();
      for tag in syslibs::active(attrs, Ecosystem::Go, &ctx.deps).tags {
         if !tags.contains(&tag) {
            tags.push(tag);
         }
      }
      let mut ld = vec!["-B gobuildid"];
      if options.cgo {
         ld.push("-linkmode=external");
      }
      if !ctx.spec()?.debug {
         ld.extend(["-s", "-w"]);
      }
      ld.extend(strs(&options.ldflags));
      let mut args = Vec::new();
      if !tags.is_empty() {
         args.push(format!("-tags={}", tags.join(",")));
      }
      args.push(format!("-ldflags={}", ld.join(" ")));
      args.extend(options.flags.iter().cloned());
      Ok(args)
   }

   fn install(ctx: &Ctx) -> io::Result<()> {
      let built = ctx.build.join("bin");
      let wanted = ctx.spec()?.bin;
      let names = file_names(&built)?;
      let out = ctx.out.join("bin");
      create_dir_all(&out)?;
      for name in names
         .iter()
         .filter(|name| wanted.as_ref().is_none_or(|list| list.contains(name)))
      {
         copy(built.join(name), out.join(name))?;
      }
      Ok(())
   }
}

/// Where `test-prepare` records each test binary's package.
const BINARIES: &str = ".bigtop-go-tests.json";

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Listed {
   import_path:     String,
   name:            String,
   dir:             PathBuf,
   #[serde(default)]
   test_go_files:   Vec<String>,
   #[serde(default)]
   x_test_go_files: Vec<String>,
}

#[derive(Deserialize, Serialize)]
struct Binary {
   package: String,
   dir:     PathBuf,
}

fn packages() -> Option<&'static BTreeMap<PathBuf, Binary>> {
   static PACKAGES: OnceLock<Option<BTreeMap<PathBuf, Binary>>> = OnceLock::new();
   PACKAGES
      .get_or_init(|| {
         let bytes = read(Path::new(&build_top().ok()?).join(BINARIES)).ok()?;
         serde_json::from_slice(&bytes).ok()
      })
      .as_ref()
}

/// A package's tests are its test binary and what that starts.
impl Tests for Go {
   fn unit(&self, exe: &Path, _args: &[OsString], _cwd: &Path) -> Option<String> {
      Some(packages()?.get(exe)?.package.clone())
   }

   fn prepares(&self) -> bool {
      true
   }
}
