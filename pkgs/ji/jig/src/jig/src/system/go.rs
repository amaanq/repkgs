use std::{
   collections::BTreeMap,
   fs::{
      copy,
      create_dir_all,
      read_dir,
   },
   io,
   path::PathBuf,
};

use serde::Deserialize;

use crate::{
   attrs::{
      Attrs,
      Ctx,
   },
   system::{
      command::{
         run,
         tool,
         unknown,
      },
      syslibs,
   },
};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Options {
   root:          String,
   tags:          Vec<String>,
   ldflags:       Vec<String>,
   packages:      Vec<String>,
   #[serde(default)]
   test_packages: Option<Vec<String>>,
   skip_tests:    Vec<String>,
   #[serde(default)]
   deps:          Option<String>,
   cgo:           bool,
   flags:         Vec<String>,
}

pub fn workdir(ctx: &Ctx) -> io::Result<PathBuf> {
   let options: Options = ctx.options("go")?;
   Ok(ctx.project_dir(&options.root))
}

/// compile, asm and link take a host-wide jigd slot like cc and rustc do.
pub fn setup(ctx: &Ctx, attrs: &Attrs) -> io::Result<BTreeMap<String, String>> {
   let options: Options = ctx.options("go")?;
   let platform = ctx.platform()?;
   let (mode, proxy) = options.deps.as_ref().map_or_else(
      || ("vendor", "off".to_owned()),
      |deps| ("mod", format!("file://{deps}")),
   );
   let env = BTreeMap::from([
      ("GOCACHE".to_owned(), format!("{}/go-cache", ctx.build)),
      ("GOPATH".to_owned(), format!("{}/go", ctx.build)),
      ("GOSUMDB".to_owned(), "off".to_owned()),
      ("GOTOOLCHAIN".to_owned(), "local".to_owned()),
      (
         "GOFLAGS".to_owned(),
         format!(
            "-mod={mode} -trimpath -buildvcs=false '-toolexec={} slot'",
            tool("jig")?.display()
         ),
      ),
      ("GOPROXY".to_owned(), proxy),
      (
         "CGO_ENABLED".to_owned(),
         if options.cgo { "1" } else { "0" }.to_owned(),
      ),
      ("GOOS".to_owned(), platform.os_names.go),
      ("GOARCH".to_owned(), platform.names.go),
   ]);
   syslibs::check(
      attrs,
      &ctx.project_dir(&options.root),
      ctx.spec()?.sys.as_deref(),
   )?;
   Ok(env)
}

pub fn phase(ctx: &Ctx, verb: &str, attrs: &Attrs) -> io::Result<()> {
   let options: Options = ctx.options("go")?;
   match verb {
      "build" => {
         let bin = format!("{}/bin/", ctx.build);
         create_dir_all(&bin)?;
         let mut args = vec![
            "build".to_owned(),
            format!("-p={}", ctx.njobs),
            "-o".to_owned(),
            bin,
         ];
         args.extend(common_args(ctx, attrs, &options)?);
         args.extend(options.packages.iter().cloned());
         run("go", &args, &[])
      },
      "test" => {
         let jobs = ctx.test_jobs()?.to_string();
         let mut args = vec![
            "test".to_owned(),
            "-p".to_owned(),
            jobs.clone(),
            "-parallel".to_owned(),
            jobs,
            "-vet=off".to_owned(),
         ];
         if !options.skip_tests.is_empty() {
            args.push("-skip".to_owned());
            args.push(options.skip_tests.join("|"));
         }
         args.extend(common_args(ctx, attrs, &options)?);
         args.extend(
            options
               .test_packages
               .as_ref()
               .unwrap_or(&options.packages)
               .iter()
               .cloned(),
         );
         run("go", &args, &[])
      },
      "install" => install(ctx),
      _ => Err(unknown("go", verb)),
   }
}

/// cgo builds link through cc so the RUNPATH and interp policy apply, and
/// gobuildid gives finish a GNU build-id for the debug split.
fn common_args(ctx: &Ctx, attrs: &Attrs, options: &Options) -> io::Result<Vec<String>> {
   let mut tags = options.tags.clone();
   for tag in syslibs::active(attrs, "go", &ctx.deps)?.tags {
      if !tags.contains(&tag) {
         tags.push(tag);
      }
   }
   let mut ld = vec!["-B gobuildid".to_owned()];
   if options.cgo {
      ld.push("-linkmode=external".to_owned());
   }
   if !ctx.spec()?.debug {
      ld.push("-s".to_owned());
      ld.push("-w".to_owned());
   }
   ld.extend(options.ldflags.iter().cloned());
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
   let mut names: Vec<String> = read_dir(&built)?
      .map(|entry| entry.map(|found| found.file_name().to_string_lossy().into_owned()))
      .collect::<io::Result<_>>()?;
   names.sort();
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
