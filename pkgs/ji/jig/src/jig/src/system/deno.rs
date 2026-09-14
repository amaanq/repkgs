use std::{
   collections::BTreeMap,
   fs::create_dir_all,
   io,
   path::{
      Path,
      PathBuf,
   },
   process::Command,
};

use serde::Deserialize;

use crate::{
   attrs::Ctx,
   launchers,
   prepare::succeed,
   system::command::{
      dep_root,
      run,
      unknown,
   },
};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Options {
   root:        String,
   deps:        String,
   entry:       BTreeMap<String, String>,
   permissions: Vec<String>,
   check:       bool,
   flags:       Vec<String>,
}

pub fn workdir(ctx: &Ctx) -> io::Result<PathBuf> {
   let options: Options = ctx.options("deno")?;
   Ok(ctx.project_dir(&options.root))
}

/// deno adds gen/ and `*_cache_v2` next to npm/ and remote/, so `DENO_DIR` is a
/// writable copy of the fetched one.
pub fn setup(ctx: &Ctx) -> io::Result<BTreeMap<String, String>> {
   let options: Options = ctx.options("deno")?;
   let deno_dir = format!("{}/deno-dir", ctx.build);
   succeed(
      Command::new("cp").args(["-r", &options.deps, &deno_dir]),
      "cp",
   )?;
   succeed(
      Command::new("chmod").args(["-R", "u+w", &deno_dir]),
      "chmod",
   )?;
   Ok(BTreeMap::from([
      ("DENO_DIR".to_owned(), deno_dir),
      ("DENO_NO_UPDATE_CHECK".to_owned(), "1".to_owned()),
      ("NO_COLOR".to_owned(), "1".to_owned()),
   ]))
}

pub fn phase(ctx: &Ctx, verb: &str) -> io::Result<()> {
   let options: Options = ctx.options("deno")?;
   match verb {
      "build" => {
         if !options.check || options.entry.is_empty() {
            return Ok(());
         }
         let mut args = vec![
            "check".to_owned(),
            "--cached-only".to_owned(),
            "--frozen".to_owned(),
         ];
         args.extend(options.entry.values().cloned());
         run("deno", &args, &[])
      },
      "test" => {
         let mut args = vec![
            "test".to_owned(),
            "--cached-only".to_owned(),
            "--frozen".to_owned(),
         ];
         args.extend(options.permissions.iter().cloned());
         args.extend(options.flags.iter().cloned());
         run("deno", &args, &[])
      },
      "install" => install(ctx, &options),
      _ => Err(unknown("deno", verb)),
   }
}

/// no `deno compile`, it splices into upstream's denort ELF, which would need
/// relinking first.
fn install(ctx: &Ctx, options: &Options) -> io::Result<()> {
   let deno = dep_root(ctx, "deno", "deno applications run on it")?;
   let app = format!("{}/lib/{}", ctx.out, ctx.spec()?.name);
   create_dir_all(format!("{}/lib", ctx.out))?;
   succeed(
      Command::new("cp").args([
         "-r",
         &ctx.project_dir(&options.root).to_string_lossy(),
         &app,
      ]),
      "cp",
   )?;
   succeed(
      Command::new("cp").args(["-r", &options.deps, &format!("{app}/deno-dir")]),
      "cp",
   )?;
   let config: Vec<String> = ["deno.json", "deno.jsonc"]
      .iter()
      .map(|name| format!("{app}/{name}"))
      .filter(|path| Path::new(path).exists())
      .map(|path| format!("--config={path}"))
      .collect();
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
      launchers::write(
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
