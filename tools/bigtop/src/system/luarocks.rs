use std::{
   fs::read_to_string,
   io,
   path::Path,
};

use mummer::{
   files::write_file,
   tree::writable,
};

use crate::{
   attrs::{
      Attrs,
      Ctx,
      utf8,
   },
   schema::{
      Field,
      PackagePath,
   },
   system::{
      command::{
         dep_root,
         file_names,
         run,
         strs,
         unknown,
      },
      dispatch::System,
   },
};

options! {
   Options {
      _root ("root"): String = ".".to_owned()
         => "directory below the source the project lives in (monorepos, build/cmake)",
      deps ("deps"): PackagePath
         => "rock server directory, by default the shared set from locks/luarocks.toml",
      rockspec ("rockspec"): Option<String> = None
         => "rockspec file when the source has several (null lets luarocks pick)",
      flags ("flags"): Vec<String> = Vec::new()
         => "extra arguments for luarocks make",
   }
}

pub struct Luarocks;

impl System for Luarocks {
   fn name(&self) -> &'static str {
      "luarocks"
   }

   fn options(&self) -> io::Result<Vec<Field>> {
      Options::schema(self.name())
   }

   // luarocks make builds into --tree
   fn phases(&self) -> &'static [&'static str] {
      &["install"]
   }

   fn phase(&self, ctx: &Ctx, _attrs: &Attrs, verb: &str, _socket: &Path) -> io::Result<()> {
      let options = ctx.options::<Options>(self.name())?;
      match verb {
         "install" => Self::install(ctx, &options),
         _ => Err(unknown(self.name(), verb)),
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
         utf8(&ctx.out)?,
         &server,
         "--deps-mode",
         "one",
         "--no-doc",
         &lua_dir,
         "CFLAGS=-O2 -fPIC",
      ];

      if ctx.platform()?.os == "macos" {
         args.push("LIBFLAG=-bundle -undefined dynamic_lookup");
      }

      args.extend(strs(&options.flags));
      args.extend(options.rockspec.as_deref());
      run("luarocks", &args, &[])?;
      let bin = ctx.out.join("bin");
      let Ok(names) = file_names(&bin) else {
         return Ok(());
      };
      for name in names {
         let path = bin.join(name);
         let mut text = read_to_string(&path)?;
         if let Some(start) = text.find("LUAROCKS_SYSCONFDIR='")
            && let Some(length) = text[start..].find("' ")
         {
            text.replace_range(start..start + length + 2, "");
         }
         let rewritten = text
            .replacen("\nexec ", "\ntree=$(cd \"${0%/*}/..\" && pwd)\nexec ", 1)
            .replace(utf8(&ctx.out)?, "'\"$tree\"'");
         writable(&path)?;
         write_file(&path, rewritten.as_bytes(), None)?;
      }
      Ok(())
   }
}
