use std::{
   env::var,
   fs::{
      copy,
      read,
   },
   io,
   path::{
      Path,
      PathBuf,
   },
   slice,
};

use jig::{
   files::write_file,
   tree::{
      find,
      note,
      replace_all,
      walk,
      writable,
   },
};
use serde::{
   Deserialize,
   Serialize,
};

use crate::{
   attrs::{
      Attrs,
      Ctx,
   },
   probe,
   site::site,
   system::{
      command::{
         Env,
         run,
         strs,
         tool,
         unknown,
      },
      dispatch::System,
      make::{
         self,
         shell,
      },
   },
};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Options {
   root:             String,
   flags:            Vec<String>,
   make_flags:       Vec<String>,
   install_flags:    Vec<String>,
   build_target:     Vec<String>,
   test_target:      Vec<String>,
   install_target:   Vec<String>,
   configure_script: String,
   out_of_tree:      bool,
}

#[derive(Serialize)]
struct Extra<'options> {
   script: &'options str,
   flags:  &'options [String],
   host:   &'options [String],
}

pub struct Autotools;

impl System for Autotools {
   fn name(&self) -> &'static str {
      "autotools"
   }

   fn workdir(&self, ctx: &Ctx) -> io::Result<PathBuf> {
      let options: Options = ctx.options("autotools")?;
      Ok(if options.out_of_tree {
         PathBuf::from(&ctx.build)
      } else {
         ctx.project_dir(&options.root)
      })
   }

   fn setup(&self, _ctx: &Ctx, _attrs: &Attrs, _socket: &Path) -> io::Result<Env> {
      Ok(shell()?.into())
   }

   fn phase(&self, ctx: &Ctx, _attrs: &Attrs, verb: &str, socket: &Path) -> io::Result<()> {
      let options: Options = ctx.options("autotools")?;
      match verb {
         "configure" => Self::configure(ctx, &options, socket),
         "build" => make::build(ctx, &options.make_flags, &options.build_target),
         "test" => make::test(ctx, &options.make_flags, &options.test_target),
         "install" => {
            let mut flags = options.make_flags;
            flags.extend(options.install_flags);
            make::install(ctx, &flags, &options.install_target)?;
            Self::unrecord_install_tool(ctx)
         },
         _ => Err(unknown("autotools", verb)),
      }
   }
}

impl Autotools {
   fn configure(ctx: &Ctx, options: &Options, socket: &Path) -> io::Result<()> {
      let script = format!("{}/{}", ctx.src, options.configure_script);
      let platform = ctx.platform()?;
      let host_flags: Vec<String> = if platform.cross {
         vec![
            format!("--host={}", platform.gnu_triple),
            format!("--build={}", platform.build_triple),
         ]
      } else {
         Vec::new()
      };
      let cache = format!("{}/config.cache", ctx.build);
      let key = probe::key(
         "autotools",
         slice::from_ref(&script),
         Some(&Extra {
            script: &options.configure_script,
            flags:  &options.flags,
            host:   &host_flags,
         }),
         false,
         socket,
      )?;
      let restored = probe::restore(&key, Path::new(&cache), socket)?;
      note("config.cache", if restored { "restored" } else { "cold" });
      Self::gnulib_gettext_literal(Path::new(&ctx.src))?;
      copy(tool("install")?, Self::install_tool(ctx))?;
      let mut args = vec![
         script.as_str(),
         "--disable-nls",
         "--disable-dependency-tracking",
         "--disable-static",
         "--enable-shared",
      ];
      args.extend(strs(&host_flags));
      args.extend(strs(&options.flags));
      let install = format!("{} -c", Self::install_tool(ctx).display());
      let config_site = site(ctx, Path::new(&script))?;
      let env = [
         ("PKGS_PREFIX", ctx.out.as_str()),
         ("PKGS_CONFIG_CACHE", &cache),
         ("INSTALL", &install),
         ("CONFIG_SITE", &config_site),
      ];
      run(&var("CONFIG_SHELL").unwrap_or_default(), &args, &env)?;
      probe::store(&key, Path::new(&cache), socket)
   }

   // gnulib's gettext.h without NLS is `((void) d, gettext (s))`, clang sees no
   // string literal behind the comma operator and -Werror=format-security
   // rejects every _("...")
   fn gnulib_gettext_literal(src: &Path) -> io::Result<()> {
      const FALLBACK: &[u8] = b"((void) (Domainname), gettext (Msgid))";
      const DOMAIN: &[u8] = b"((void) (Category), dgettext (Domainname, Msgid))";
      for entry in walk(src)? {
         if entry.kind != 'f' || !entry.rel.ends_with("gettext.h") && entry.rel != "gettext.h" {
            continue;
         }
         let path = Path::new(&entry.path);
         let bytes = read(path)?;
         if find(&bytes, FALLBACK).is_none() {
            continue;
         }
         let fixed = replace_all(
            &replace_all(&bytes, FALLBACK, b"gettext (Msgid)"),
            DOMAIN,
            b"dgettext (Domainname, Msgid)",
         );
         writable(path)?;
         write_file(path, &fixed, None)?;
      }
      Ok(())
   }

   /// configure's INSTALL.
   fn install_tool(ctx: &Ctx) -> PathBuf {
      PathBuf::from(format!("{}/install", ctx.build))
   }

   fn unrecord_install_tool(ctx: &Ctx) -> io::Result<()> {
      let recorded = Self::install_tool(ctx);
      let needle = recorded.as_os_str().as_encoded_bytes();
      let out = Path::new(&ctx.out);
      if !out.exists() {
         return Ok(());
      }
      for entry in walk(out)? {
         if entry.kind != 'f' {
            continue;
         }
         let path = Path::new(&entry.path);
         let bytes = read(path)?;
         if bytes.contains(&0) || find(&bytes, needle).is_none() {
            continue;
         }
         writable(path)?;
         write_file(path, &replace_all(&bytes, needle, b"install"), None)?;
      }
      Ok(())
   }
}
