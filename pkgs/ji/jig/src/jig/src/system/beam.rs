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
   path::{
      Path,
      PathBuf,
   },
   process::{
      Command,
      Stdio,
   },
};

use serde::Deserialize;

use crate::{
   attrs::{
      Attrs,
      Ctx,
   },
   files::write_file,
   launchers,
   probe,
   store::on_path,
   system::command::{
      dep_root,
      run,
      tool_root,
      unknown,
   },
   tree::note,
};

#[derive(Clone, Copy)]
pub enum Tool {
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

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Options {
   root:    String,
   deps:    String,
   #[serde(default)]
   escript: Vec<String>,
   flags:   Vec<String>,
}

pub fn workdir(tool: Tool, ctx: &Ctx) -> io::Result<PathBuf> {
   let options: Options = ctx.options(tool.name())?;
   Ok(ctx.project_dir(&options.root))
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
   launchers::write(
      ctx,
      &name,
      &format!("{erlang}/bin/escript"),
      &[placed],
      &BTreeMap::new(),
   )
}

fn deps_key(tool: Tool, options: &Options, socket: &Path) -> io::Result<String> {
   let set = Path::new(&options.deps)
      .file_name()
      .map(|name| name.to_string_lossy().into_owned())
      .unwrap_or_default();
   probe::key(
      &format!("{}-deps/{set}", tool.name()),
      &[],
      None::<&()>,
      false,
      socket,
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

/// rebar3 takes _checkouts/ over the lock without asking a registry, mix
/// wants deps/ the way `mix deps.get` leaves it, with a .hex manifest that
/// `Hex.SCM.lock_status` accepts. Compiled deps round-trip through jigd.
pub fn setup(
   tool: Tool,
   ctx: &Ctx,
   attrs: &Attrs,
   socket: &Path,
) -> io::Result<BTreeMap<String, String>> {
   let options: Options = ctx.options(tool.name())?;
   let vars = env(tool, ctx, attrs)?;
   let packages = format!("{}/packages/hexpm", options.deps);
   let (compiled, kind) = match tool {
      Tool::Rebar3 => {
         for tar in sorted_names(&packages)?.iter().filter(|name| {
            Path::new(name)
               .extension()
               .is_some_and(|extension| extension.eq_ignore_ascii_case("tar"))
         }) {
            let app = tar.rsplit_once('-').map_or(tar.as_str(), |split| split.0);
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
   let key = deps_key(tool, &options, socket)?;
   let restored = probe::restore_dir(&key, Path::new(compiled), socket)?;
   note(kind, if restored { "restored" } else { "cold" });
   if matches!(tool, Tool::Mix) {
      let borrowed: Vec<(&str, String)> = vars
         .iter()
         .map(|pair| (pair.0.as_str(), pair.1.clone()))
         .collect();
      run(
         "mix",
         &[
            "deps.compile".to_owned(),
            "--skip-umbrella-children".to_owned(),
         ],
         &borrowed,
      )?;
      probe::store_dir(&key, Path::new(compiled), socket)?;
   }
   Ok(vars.into_iter().collect())
}

pub fn phase(tool: Tool, ctx: &Ctx, verb: &str, socket: &Path) -> io::Result<()> {
   let options: Options = ctx.options(tool.name())?;
   match (tool, verb) {
      (Tool::Rebar3, "build") => {
         let mut args = vec!["compile".to_owned()];
         args.extend(options.flags.iter().cloned());
         run("rebar3", &args, &[])?;
         probe::store_dir(
            &deps_key(tool, &options, socket)?,
            Path::new("_build/default/checkouts"),
            socket,
         )
      },
      (Tool::Rebar3, "test") => run("rebar3", &["eunit".to_owned()], &[]),
      (Tool::Rebar3, "install") => {
         run("rebar3", &["escriptize".to_owned()], &[])?;
         for name in sorted_names("_build/default/bin")? {
            install_escript(ctx, &format!("_build/default/bin/{name}"))?;
         }
         Ok(())
      },
      (Tool::Mix, "build") => {
         let mut args = vec!["compile".to_owned(), "--no-deps-check".to_owned()];
         args.extend(options.flags.iter().cloned());
         run("mix", &args, &[])
      },
      (Tool::Mix, "test") => {
         run(
            "mix",
            &["test".to_owned(), "--no-deps-check".to_owned()],
            &[],
         )
      },
      (Tool::Mix, "install") => mix_install(ctx, &options),
      _ => Err(unknown(tool.name(), verb)),
   }
}

fn mix_install(ctx: &Ctx, options: &Options) -> io::Result<()> {
   if !options.escript.is_empty() {
      run(
         "mix",
         &["escript.build".to_owned(), "--no-deps-check".to_owned()],
         &[],
      )?;
      for escript in &options.escript {
         install_escript(ctx, escript)?;
      }
      return Ok(());
   }
   let name = ctx.spec()?.name;
   let release = format!("{}/lib/{name}", ctx.out);
   run(
      "mix",
      &[
         "release".to_owned(),
         "--overwrite".to_owned(),
         "--path".to_owned(),
         release.clone(),
      ],
      &[],
   )?;
   let bin = format!("{}/bin", ctx.out);
   create_dir_all(&bin)?;
   for file in sorted_names(&format!("{release}/bin"))? {
      symlink(format!("../lib/{name}/bin/{file}"), format!("{bin}/{file}"))?;
   }
   Ok(())
}
