use std::{
   fs::{
      copy,
      create_dir_all,
   },
   io,
   path::Path,
};

use mummer::syslibs::Ecosystem;

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
      root ("root"): String = ".".to_owned()
         => "directory below the source the project lives in (monorepos, build/cmake)",
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
   }
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
   fn setup(&self, ctx: &Ctx, attrs: &Attrs, _socket: &Path) -> io::Result<Env> {
      let options: Options = ctx.options(self.name())?;
      let platform = ctx.platform()?;
      let (mode, proxy) = options.deps.as_deref().map_or_else(
         || ("vendor", "off".to_owned()),
         |deps| ("mod", format!("file://{deps}")),
      );
      syslibs::check(
         attrs,
         &ctx.project_dir(&options.root),
         ctx.spec()?.sys.as_deref(),
         None,
      )?;
      Ok([
         ("GOCACHE", format!("{}/go-cache", ctx.build)),
         ("GOPATH", format!("{}/go", ctx.build)),
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
      ]
      .into())
   }

   fn phase(&self, ctx: &Ctx, attrs: &Attrs, verb: &str, _socket: &Path) -> io::Result<()> {
      let options: Options = ctx.options(self.name())?;
      match verb {
         "build" => {
            let bin = format!("{}/bin/", ctx.build);
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
   /// cgo builds link through cc so the RUNPATH and interp policy apply, and
   /// gobuildid gives finish a GNU build-id for the debug split.
   fn common_args(ctx: &Ctx, attrs: &Attrs, options: &Options) -> io::Result<Vec<String>> {
      let mut tags = options.tags.clone();
      for tag in syslibs::active(attrs, Ecosystem::Go, &ctx.deps)?.tags {
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
      let built = format!("{}/bin", ctx.build);
      let wanted = ctx.spec()?.bin;
      let names = file_names(Path::new(&built))?;
      let out = format!("{}/bin", ctx.out);
      create_dir_all(&out)?;
      for name in names
         .iter()
         .filter(|name| wanted.as_ref().is_none_or(|list| list.contains(name)))
      {
         copy(format!("{built}/{name}"), format!("{out}/{name}"))?;
      }
      Ok(())
   }
}
