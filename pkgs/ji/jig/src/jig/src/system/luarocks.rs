use std::{
   fs::{
      read_dir,
      read_to_string,
   },
   io,
   path::{
      Path,
      PathBuf,
   },
};

use serde::Deserialize;

use crate::{
   attrs::Ctx,
   files::write_file,
   system::command::{
      dep_root,
      run,
      unknown,
   },
   tree::writable,
};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Options {
   root:     String,
   deps:     String,
   #[serde(default)]
   rockspec: Option<String>,
   flags:    Vec<String>,
}

pub fn workdir(ctx: &Ctx) -> io::Result<PathBuf> {
   let options: Options = ctx.options("luarocks")?;
   Ok(ctx.project_dir(&options.root))
}

pub fn phase(ctx: &Ctx, verb: &str) -> io::Result<()> {
   let options: Options = ctx.options("luarocks")?;
   match verb {
      "install" => install(ctx, &options),
      _ => Err(unknown("luarocks", verb)),
   }
}

/// luarocks' bin wrappers name its own config dir, a build tool nothing reads
/// at run time, and the tree by absolute path, and the tree is where the
/// wrapper is.
fn install(ctx: &Ctx, options: &Options) -> io::Result<()> {
   let lua = dep_root(ctx, "lua", "rocks run on the target lua")?;
   let mut args = vec![
      "make".to_owned(),
      "--tree".to_owned(),
      ctx.out.clone(),
      format!("--only-server={}", options.deps),
      "--deps-mode".to_owned(),
      "one".to_owned(),
      "--no-doc".to_owned(),
      format!("LUA_DIR={lua}"),
      "CFLAGS=-O2 -fPIC".to_owned(),
   ];
   args.extend(options.flags.iter().cloned());
   args.extend(options.rockspec.iter().cloned());
   run("luarocks", &args, &[])?;
   let bin = format!("{}/bin", ctx.out);
   let Ok(entries) = read_dir(&bin) else {
      return Ok(());
   };
   let mut names: Vec<String> = entries
      .map(|entry| entry.map(|found| found.file_name().to_string_lossy().into_owned()))
      .collect::<io::Result<_>>()?;
   names.sort();
   for name in names {
      let path = format!("{bin}/{name}");
      let mut text = read_to_string(&path)?;
      if let Some(start) = text.find("LUAROCKS_SYSCONFDIR='")
         && let Some(length) = text[start..].find("' ")
      {
         text.replace_range(start..start + length + 2, "");
      }
      let rewritten = text
         .replacen("\nexec ", "\ntree=$(cd \"${0%/*}/..\" && pwd)\nexec ", 1)
         .replace(&ctx.out, "'\"$tree\"'");
      writable(Path::new(&path))?;
      write_file(Path::new(&path), rewritten.as_bytes(), None)?;
   }
   Ok(())
}
