use std::{
   collections::BTreeMap,
   fs::{
      copy,
      create_dir_all,
   },
   io,
   path::{
      Path,
      PathBuf,
   },
   process::Command,
};

use mummer::{
   process::Checked as _,
   syslibs::Ecosystem,
   tree::note,
};
use serde::Deserialize;

use crate::{
   attrs::{
      Attrs,
      Ctx,
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
   fn name(&self) -> &'static str {
      "go"
   }

   fn options(&self) -> io::Result<Vec<Field>> {
      Options::schema(self.name())
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
         "test" => {
            let jobs = ctx.test_jobs()?.to_string();
            let skip = options.skip_tests.join("|");
            let common = Self::common_args(ctx, attrs, &options)?;
            let mut args = vec!["test", "-p", &jobs, "-parallel", &jobs, "-vet=off"];
            if !options.skip_tests.is_empty() {
               args.extend(["-skip", &skip]);
            }
            args.extend(strs(&common));
            args.extend(strs(
               options.test_packages.as_ref().unwrap_or(&options.packages),
            ));
            run("go", &args, &[])
         },
         "install" => Self::install(ctx),
         _ => Err(unknown(self.name(), verb)),
      }
   }
}

impl Go {
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
