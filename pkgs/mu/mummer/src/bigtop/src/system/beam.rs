use std::{
   collections::BTreeMap,
   fs::{
      copy,
      create_dir_all,
      read_dir,
      read_to_string,
   },
   io,
   os::unix::fs::symlink,
   path::Path,
   process::{
      Command,
      Stdio,
   },
};

use mummer::{
   files::write_file,
   store::on_path,
   tree::note,
};

use crate::{
   attrs::{
      Attrs,
      Ctx,
   },
   launchers::entries::write,
   probe,
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
         tool_root,
         unknown,
      },
      dispatch::System,
   },
};

#[derive(Clone, Copy)]
enum Tool {
   Rebar3,
   Mix,
}

impl Tool {
   const fn name(self) -> &'static str {
      match self {
         Self::Rebar3 => "rebar3",
         Self::Mix => "mix",
      }
   }
}

macro_rules! tools {
   ($($name:ident => $tool:expr),* $(,)?) => {
      $(
         pub struct $name;

         impl System for $name {
            fn name(&self) -> &'static str {
               $tool.name()
            }

            fn options(&self) -> io::Result<Vec<Field>> {
               if matches!($tool, Tool::Mix) {
                  MixOptions::schema(self.name())
               } else {
                  Options::schema(self.name())
               }
            }

            // no test phase by default: MIX_ENV=test deps are outside the prod lock subset, eunit/ct
            // deps live in the test profile, outside rebar.lock
            fn phases(&self) -> &'static [&'static str] {
               &["build", "install"]
            }

            fn setup(&self, ctx: &Ctx, attrs: &Attrs, socket: &Path) -> io::Result<Env> {
               setup($tool, ctx, attrs, socket)
            }

            fn phase(&self, ctx: &Ctx, _attrs: &Attrs, verb: &str, socket: &Path) -> io::Result<()> {
               phase($tool, ctx, verb, socket)
            }
         }
      )*
   };
}

tools! {
   Rebar3 => Tool::Rebar3,
   Mix => Tool::Mix,
}

options! {
   Options {
      _root ("root"): String = ".".to_owned()
         => "directory below the source the project lives in (monorepos, build/cmake)",
      deps ("deps"): PackagePath
         => "the locked dependencies, fetched (fetch.hexDeps) from the package's own lock file",
      flags ("flags"): Vec<String> = Vec::new()
         => "extra arguments for rebar3 compile",
   }
}

options! {
   MixOptions {
      _root ("root"): String = ".".to_owned()
         => "directory below the source the project lives in (monorepos, build/cmake)",
      deps ("deps"): PackagePath
         => "the locked dependencies, fetched (fetch.hexDeps) from the package's own lock file",
      flags ("flags"): Vec<String> = Vec::new()
         => "extra arguments for mix compile",
      escript ("escript"): Vec<String> = Vec::new()
         => "escripts `mix escript.build` writes, installed into bin/. Empty means a mix release",
   }
}

/// mix's `escript` is the one option rebar3 lacks.
fn options(tool: Tool, ctx: &Ctx) -> io::Result<(Options, Vec<String>)> {
   if !matches!(tool, Tool::Mix) {
      return Ok((ctx.options(tool.name())?, Vec::new()));
   }

   let mix: MixOptions = ctx.options("mix")?;
   let common = Options {
      _root: mix._root,
      deps:  mix.deps,
      flags: mix.flags,
   };
   Ok((common, mix.escript))
}

fn sorted_names(dir: &str) -> io::Result<Vec<String>> {
   let mut names: Vec<String> = read_dir(dir)?
      .map(|entry| entry.map(|found| found.file_name().to_string_lossy().into_owned()))
      .collect::<io::Result<_>>()?;
   names.sort();
   Ok(names)
}

/// a Hex tarball's contents.tar.gz into `dest`.
fn unpack(tar: &str, dest: &str) -> io::Result<()> {
   create_dir_all(dest)?;
   let mut outer = Command::new("bsdtar")
      .args(["-xOf", tar, "contents.tar.gz"])
      .stdout(Stdio::piped())
      .spawn()?;
   let stdout = outer
      .stdout
      .take()
      .ok_or_else(|| io::Error::other("bsdtar has no stdout"))?;
   let inner = Command::new("bsdtar")
      .args(["-xzf", "-", "-C", dest])
      .stdin(stdout)
      .status()?;
   if !outer.wait()?.success() || !inner.success() {
      return Err(io::Error::other(format!("unpacking {tar} failed")));
   }
   Ok(())
}

/// escript derives the module to call from the file name, so an escript keeps
/// its name under lib/escript/ and the launcher runs the target's escript on
/// it.
fn install_escript(ctx: &Ctx, file: &str) -> io::Result<()> {
   let name = Path::new(file)
      .file_name()
      .map(|name| name.to_string_lossy().into_owned())
      .unwrap_or_default();
   let dir = format!("{}/lib/escript", ctx.out);
   create_dir_all(&dir)?;
   let placed = format!("{dir}/{name}");
   copy(file, &placed)?;
   let erlang = dep_root(ctx, "erlang", "escripts run under the target erlang")?;
   write(
      ctx,
      &name,
      &format!("{erlang}/bin/escript"),
      &[placed],
      &BTreeMap::new(),
   )
}

fn deps_key(tool: Tool, options: &Options) -> io::Result<String> {
   let set = Path::new(&*options.deps)
      .file_name()
      .map(|name| name.to_string_lossy().into_owned())
      .unwrap_or_default();
   probe::key(
      &format!("{}-deps/{set}", tool.name()),
      &[],
      None::<&()>,
      false,
   )
}

struct Locked {
   app:     String,
   name:    String,
   version: String,
   inner:   String,
}

/// mix.lock lines read `"app": {:hex, :name, "version", "inner", …}`.
fn mix_lock(text: &str) -> Vec<Locked> {
   let mut found = Vec::new();
   let mut segments = text.split("{:hex, :");
   let mut previous = segments.next().unwrap_or_default();
   for segment in segments {
      let app_field = previous
         .trim_end()
         .strip_suffix(':')
         .and_then(|rest| rest.strip_suffix('"'))
         .and_then(|rest| rest.rsplit('"').next());
      let mut fields = segment.splitn(5, '"');
      let name_field = fields.next().and_then(|rest| rest.split(',').next());
      let version_field = fields.next();
      let inner_field = fields.nth(1);
      if let (Some(app), Some(name), Some(version), Some(inner)) =
         (app_field, name_field, version_field, inner_field)
         && inner.len() == 64
         && inner.bytes().all(|byte| byte.is_ascii_hexdigit())
      {
         found.push(Locked {
            app:     app.to_owned(),
            name:    name.to_owned(),
            version: version.to_owned(),
            inner:   inner.to_owned(),
         });
      }
      previous = segment;
   }
   found
}

fn env(tool: Tool, ctx: &Ctx, attrs: &Attrs) -> io::Result<Vec<(String, String)>> {
   let shared = [
      ("ERL_AFLAGS", "+B".to_owned()),
      ("LANG", "C.UTF-8".to_owned()),
      ("ERL_COMPILER_OPTIONS", "deterministic".to_owned()),
   ];
   let own: Vec<(&str, String)> = match tool {
      Tool::Rebar3 => {
         vec![
            ("HOME", ctx.build.clone()),
            ("REBAR_CACHE_DIR", format!("{}/rebar3-cache", ctx.build)),
            ("REBAR_OFFLINE", "1".to_owned()),
         ]
      },
      Tool::Mix => {
         let rebar3 = on_path("rebar3");
         vec![
            ("MIX_ENV", "prod".to_owned()),
            ("MIX_HOME", format!("{}/mix-home", ctx.build)),
            (
               "MIX_ARCHIVES",
               format!("{}/lib/archives", tool_root(attrs, "hex")?),
            ),
            (
               "MIX_REBAR3",
               if rebar3.is_absolute() {
                  rebar3.to_string_lossy().into_owned()
               } else {
                  String::new()
               },
            ),
            ("MIX_OS_DEPS_COMPILE_PARTITION_COUNT", ctx.njobs.to_string()),
            ("HEX_HOME", format!("{}/hex-home", ctx.build)),
            ("HEX_OFFLINE", "1".to_owned()),
         ]
      },
   };
   Ok(own
      .into_iter()
      .chain(shared)
      .map(|pair| (pair.0.to_owned(), pair.1))
      .collect())
}

fn checkout_name(tar: &str) -> &str {
   let mut at = 0;
   while let Some(offset) = tar[at..].find('-') {
      let cut = at + offset;
      if tar[cut + 1..].starts_with(|found: char| found.is_ascii_digit()) {
         return &tar[..cut];
      }
      at = cut + 1;
   }
   tar
}

/// rebar3 takes _checkouts/ over the lock without asking a registry, mix
/// wants deps/ the way `mix deps.get` leaves it, with a .hex manifest that
/// `Hex.SCM.lock_status` accepts. Compiled deps round-trip through mummerd.
fn setup(tool: Tool, ctx: &Ctx, attrs: &Attrs, socket: &Path) -> io::Result<Env> {
   let (options, _escripts) = options(tool, ctx)?;
   let vars = env(tool, ctx, attrs)?;
   let packages = format!("{}/packages/hexpm", options.deps);
   let (compiled, kind) = match tool {
      Tool::Rebar3 => {
         for tar in sorted_names(&packages)?.iter().filter(|name| {
            Path::new(name)
               .extension()
               .is_some_and(|extension| extension.eq_ignore_ascii_case("tar"))
         }) {
            let app = checkout_name(tar);
            unpack(&format!("{packages}/{tar}"), &format!("_checkouts/{app}"))?;
         }
         ("_build/default/checkouts", "rebar3-deps")
      },
      Tool::Mix => {
         for dep in mix_lock(&read_to_string("mix.lock")?) {
            let dest = format!("deps/{}", dep.app);
            unpack(
               &format!("{packages}/{}-{}.tar", dep.name, dep.version),
               &dest,
            )?;
            write_file(
               Path::new(&format!("{dest}/.hex")),
               format!("{},{},{},hexpm", dep.name, dep.version, dep.inner).as_bytes(),
               None,
            )?;
         }
         ("_build/prod/lib", "mix-deps")
      },
   };
   let key = deps_key(tool, &options)?;
   let restored = probe::restore_dir(&key, Path::new(compiled), socket)?;
   note(kind, if restored { "restored" } else { "cold" });
   if matches!(tool, Tool::Mix) {
      let env: Vec<(&str, &str)> = vars
         .iter()
         .map(|pair| (pair.0.as_str(), pair.1.as_str()))
         .collect();
      run("mix", &["deps.compile", "--skip-umbrella-children"], &env)?;
      probe::store_dir(&key, Path::new(compiled), socket)?;
   }
   Ok(Env::from(
      vars.into_iter().collect::<BTreeMap<String, String>>(),
   ))
}

fn phase(tool: Tool, ctx: &Ctx, verb: &str, socket: &Path) -> io::Result<()> {
   let (options, escripts) = options(tool, ctx)?;
   match (tool, verb) {
      (Tool::Rebar3, "build") => {
         let mut args = vec!["compile"];
         args.extend(strs(&options.flags));
         run("rebar3", &args, &[])?;
         probe::store_dir(
            &deps_key(tool, &options)?,
            Path::new("_build/default/checkouts"),
            socket,
         )
      },
      (Tool::Rebar3, "test") => run("rebar3", &["eunit"], &[]),
      (Tool::Rebar3, "install") => {
         run("rebar3", &["escriptize"], &[])?;
         for name in sorted_names("_build/default/bin")? {
            install_escript(ctx, &format!("_build/default/bin/{name}"))?;
         }
         Ok(())
      },
      (Tool::Mix, "build") => {
         let mut args = vec!["compile", "--no-deps-check"];
         args.extend(strs(&options.flags));
         run("mix", &args, &[])
      },
      (Tool::Mix, "test") => run("mix", &["test", "--no-deps-check"], &[]),
      (Tool::Mix, "install") => mix_install(ctx, &escripts),
      _ => Err(unknown(tool.name(), verb)),
   }
}

fn mix_install(ctx: &Ctx, escripts: &[String]) -> io::Result<()> {
   if !escripts.is_empty() {
      run("mix", &["escript.build", "--no-deps-check"], &[])?;
      for escript in escripts {
         install_escript(ctx, escript)?;
      }
      return Ok(());
   }
   let name = ctx.spec()?.name;
   let release = format!("{}/lib/{name}", ctx.out);
   run("mix", &["release", "--overwrite", "--path", &release], &[])?;
   let bin = format!("{}/bin", ctx.out);
   create_dir_all(&bin)?;
   for file in sorted_names(&format!("{release}/bin"))? {
      symlink(format!("../lib/{name}/bin/{file}"), format!("{bin}/{file}"))?;
   }
   Ok(())
}
