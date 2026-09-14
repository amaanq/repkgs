use std::{
   fs::{
      read_dir,
      read_to_string,
   },
   io,
   path::Path,
};

use serde::Deserialize;

use crate::{
   attrs::{
      Attrs,
      Ctx,
   },
   files::write_file,
   system::{
      command::{
         dep_root,
         run,
         strs,
         unknown,
      },
      dispatch::System,
   },
   tree::writable,
};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Options {
   deps:     String,
   #[serde(default)]
   rockspec: Option<String>,
   flags:    Vec<String>,
}

pub struct Luarocks;

impl System for Luarocks {
   fn name(&self) -> &'static str {
      "luarocks"
   }

   fn phase(&self, ctx: &Ctx, _attrs: &Attrs, verb: &str, _socket: &Path) -> io::Result<()> {
      let options: Options = ctx.options("luarocks")?;
      match verb {
         "install" => Self::install(ctx, &options),
         _ => Err(unknown("luarocks", verb)),
      }
   }
}

impl Luarocks {
   /// luarocks' bin wrappers name its own config dir, a build tool nothing
   /// reads at run time, and the tree by absolute path, and the tree is
   /// where the wrapper is.
   fn install(ctx: &Ctx, options: &Options) -> io::Result<()> {
      let lua = dep_root(ctx, "lua", "rocks run on the target lua")?;
      let server = format!("--only-server={}", options.deps);
      let lua_dir = format!("LUA_DIR={lua}");
      let mut args = vec![
         "make",
         "--tree",
         &ctx.out,
         &server,
         "--deps-mode",
         "one",
         "--no-doc",
         &lua_dir,
         "CFLAGS=-O2 -fPIC",
      ];
      args.extend(strs(&options.flags));
      args.extend(options.rockspec.as_deref());
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
}
