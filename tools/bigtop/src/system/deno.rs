use std::{
   collections::BTreeMap,
   fs::create_dir_all,
   io,
   path::Path,
   process::Command,
};

use crate::{
   attrs::{
      Attrs,
      Ctx,
   },
   launchers::entries::write,
   prepare::succeed,
   schema::{
      Field,
      PackagePath,
   },
   system::{
      command::{
         Env,
         dep_root,
         run,
         strs,
         unknown,
      },
      dispatch::System,
   },
};

options! {
   Options {
      root ("root"): String = ".".to_owned()
         => "directory below the source the project lives in (monorepos, build/cmake)",
      deps ("deps"): PackagePath
         => "the locked dependencies, fetched (fetch.denoDeps) from the package's own lock file",
      entry ("entry"): BTreeMap<String, String> = BTreeMap::new()
         => "bin name -> module path, each becomes bin/<name> running `deno run` on it",
      permissions ("permissions"): Vec<String> = vec!["-A".to_owned()]
         => "permission flags for run and test",
      check ("check"): bool = true
         => "deno check the entry points",
      flags ("flags"): Vec<String> = Vec::new()
         => "extra arguments for deno run and deno test",
   }
}

pub struct Deno;

impl System for Deno {
   fn name(&self) -> &'static str {
      "deno"
   }

   fn options(&self) -> io::Result<Vec<Field>> {
      Options::schema(self.name())
   }

   /// deno adds gen/ and `*_cache_v2` next to npm/ and remote/, so `DENO_DIR`
   /// is a writable copy of the fetched one.
   fn setup(&self, ctx: &Ctx, _attrs: &Attrs, _socket: &Path) -> io::Result<Env> {
      let options = ctx.options::<Options>(self.name())?;
      let deno_dir = format!("{}/deno-dir", ctx.build);
      succeed(Command::new("cp").args(["-r", &options.deps, &deno_dir]))?;
      succeed(Command::new("chmod").args(["-R", "u+w", &deno_dir]))?;
      Ok([
         ("DENO_DIR", deno_dir),
         ("DENO_NO_UPDATE_CHECK", "1".to_owned()),
         ("NO_COLOR", "1".to_owned()),
      ]
      .into())
   }

   fn phase(&self, ctx: &Ctx, _attrs: &Attrs, verb: &str, _socket: &Path) -> io::Result<()> {
      let options = ctx.options::<Options>(self.name())?;
      match verb {
         "build" => {
            if !options.check || options.entry.is_empty() {
               return Ok(());
            }
            let mut args = vec!["check", "--cached-only", "--frozen"];
            args.extend(options.entry.values().map(String::as_str));
            run("deno", &args, &[])
         },
         "test" => {
            let mut args = vec!["test", "--cached-only", "--frozen"];
            args.extend(strs(&options.permissions));
            args.extend(strs(&options.flags));
            run("deno", &args, &[])
         },
         "install" => Self::install(ctx, &options),
         _ => Err(unknown(self.name(), verb)),
      }
   }
}

impl Deno {
   /// no `deno compile`, it splices into upstream's denort ELF, which would
   /// need relinking first.
   fn install(ctx: &Ctx, options: &Options) -> io::Result<()> {
      let deno = dep_root(ctx, "deno", "deno applications run on it")?;
      let app = format!("{}/lib/{}", ctx.out, ctx.spec()?.name);
      create_dir_all(format!("{}/lib", ctx.out))?;
      succeed(Command::new("cp").args([
         "-r",
         &ctx.project_dir(&options.root).to_string_lossy(),
         &app,
      ]))?;
      succeed(Command::new("cp").args(["-r", &options.deps, &format!("{app}/deno-dir")]))?;
      let config = ["deno.json", "deno.jsonc"]
         .iter()
         .map(|name| format!("{app}/{name}"))
         .filter(|path| Path::new(path).exists())
         .map(|path| format!("--config={path}"))
         .collect::<Vec<String>>();
      for (name, module) in &options.entry {
         let mut args = vec![
            "run".to_owned(),
            "--cached-only".to_owned(),
            "--frozen".to_owned(),
         ];
         args.extend(config.iter().cloned());
         args.extend(options.permissions.iter().cloned());
         args.extend(options.flags.iter().cloned());
         args.push(format!("{app}/{module}"));
         write(
            ctx,
            name,
            &format!("{deno}/bin/deno"),
            &args,
            &BTreeMap::from([
               ("DENO_DIR".to_owned(), format!("{app}/deno-dir")),
               ("DENO_NO_UPDATE_CHECK".to_owned(), "1".to_owned()),
            ]),
         )?;
      }
      Ok(())
   }
}
