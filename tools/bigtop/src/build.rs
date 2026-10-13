use std::{
   collections::BTreeMap,
   convert::Infallible,
   env::{
      args_os,
      current_exe,
      set_current_dir,
      var_os,
      vars,
   },
   fs::{
      Permissions,
      create_dir_all,
      read,
      read_dir,
      read_link,
      read_to_string,
      set_permissions,
      write,
   },
   io,
   os::unix::{
      fs::symlink,
      process::CommandExt as _,
   },
   path::{
      Path,
      PathBuf,
   },
   process::Command,
   time::Instant,
};

use mummer::{
   files::write_file,
   process::Checked as _,
   tree::{
      note,
      set_phase,
   },
};
use rustix::{
   mount::{
      MountFlags,
      mount,
      mount_remount,
   },
   process::{
      getgid,
      getuid,
   },
   thread::{
      CapabilitySet,
      UnshareFlags,
      capabilities,
      clear_ambient_capability_set,
      configure_capability_in_ambient_set,
      set_capabilities,
      unshare_unsafe,
   },
};
use serde_json::Value;
use tightrope::client::Client;

use crate::{
   attrs::{
      Attrs,
      Ctx,
      Kind,
      Step,
      build_top,
   },
   finish,
   output,
   prepare::{
      Source,
      prepare,
   },
   system::{
      command::{
         Env,
         export,
      },
      dispatch,
   },
   test_cache::{
      self,
      Ran,
   },
   trace::Tests,
};

/// the tail of every nu phase, so what the phase exported reaches the next
/// one. nu keeps PATH as a list, and its own bookkeeping is not part of the
/// build environment.
const HANDOVER: &str = r#"
$env | transpose k v | each {|r|
  let kind = ($r.v | describe | str replace -r '<.*' '')
  {k: $r.k, v: (if $kind == "string" { $r.v } else if $kind == "list" { $r.v | str join ":" } else { null })}
} | where v != null and k !~ '^(NU_|PWD$|OLDPWD$|CURRENT_FILE$|FILE_PWD$|PROCESS_PATH$)' | transpose -rd | to json | save -f $"($env.NIX_BUILD_TOP)/phase-env.json"
"#;

/// nu's own bookkeeping, not part of the build environment a phase hands on.
pub fn nu_internal(name: &str) -> bool {
   name.starts_with("NU_")
      || matches!(
         name,
         "PWD" | "OLDPWD" | "CURRENT_FILE" | "FILE_PWD" | "PROCESS_PATH"
      )
}

fn apply(env: &BTreeMap<String, Value>) {
   for (name, value) in env {
      match *value {
         Value::String(ref text) => export(name, text),
         Value::Array(ref items) => {
            let joined = items
               .iter()
               .filter_map(Value::as_str)
               .collect::<Vec<&str>>();
            export(name, &joined.join(":"));
         },
         Value::Bool(flag) => export(name, &flag.to_string()),
         Value::Number(ref number) => export(name, &number.to_string()),
         Value::Null | Value::Object(_) => {},
      }
   }
}

/// an inline phase, run under nu with the builder tree on its include path
/// and its environment taken back afterwards.
fn nu_phase(attrs: &Attrs, index: usize, name: &str, body: &str, top: &str) -> io::Result<()> {
   nu_command(attrs, index, body, top)?
      .checked()
      .map_err(|error| io::Error::other(format!("phase {name}, {error}")))?;
   handover(top)
}

fn nu_command(attrs: &Attrs, index: usize, body: &str, top: &str) -> io::Result<Command> {
   let tree = attrs
      .probe_inputs
      .first()
      .ok_or_else(|| io::Error::other("the builder tree is not among probeInputs"))?;
   let script = format!("{top}/phase-{index}.nu");
   write_file(
      Path::new(&script),
      format!("{}\n{body}\n{HANDOVER}", attrs.plan.prelude).as_bytes(),
      None,
   )?;
   let mut command = Command::new("nu");
   command.args([
      "--no-config-file",
      &format!("--include-path={tree}"),
      "--experimental-options=[cell-path-types]",
      &script,
   ]);
   Ok(command)
}

fn handover(top: &str) -> io::Result<()> {
   let handed = serde_json::from_str::<BTreeMap<String, String>>(&read_to_string(format!(
      "{top}/phase-env.json"
   ))?)?;
   let current = vars().collect::<BTreeMap<String, String>>();
   let mut removed = Env::default();
   for key in current.keys() {
      if !handed.contains_key(key) && !nu_internal(key) {
         removed.unset(key);
      }
   }
   removed.apply();
   for (key, value) in &handed {
      if current.get(key) != Some(value) {
         export(key, value);
      }
   }
   Ok(())
}

/// prepare unpacks and patches with tools from the build dependencies, before
/// it has computed the environment that names them.
pub fn run(tests: bool, from_tree: Option<&Path>, socket: &Path) -> io::Result<()> {
   let attrs = Attrs::load()?;
   let spec = attrs.spec()?;
   if spec.tests.etc == Some(true) {
      if var_os(PRIVATE_ETC).is_none() {
         match private_etc()? {}
      }
      let mut cleared = Env::default();
      cleared.unset(PRIVATE_ETC);
      cleared.apply();
      clear_ambient_capability_set()?;
   }
   let started = Instant::now();
   let cached = !tests && from_tree.is_none();
   if cached && output::restore(&attrs)? {
      note(
         "phases",
         &format!("output-cache {:.1}s", started.elapsed().as_secs_f64()),
      );
      return Ok(());
   }
   let mut ctx = setup(
      &attrs,
      from_tree.map_or(Source::Unpack, Source::Archive),
      socket,
   )?;
   let mut timings = vec![format!("prepare {:.1}s", started.elapsed().as_secs_f64())];
   let mut writable = false;
   for (index, step) in attrs.plan.phases.iter().enumerate() {
      let wanted = if tests {
         step.test
      } else {
         !step.test || (spec.tests.run.unwrap_or(true) && !attrs.plan.separate && ctx.tests_run)
      };
      if wanted {
         let editing = spec.tests.etc == Some(true) && edits_etc(&attrs.plan.phases, index);
         if editing != writable {
            etc_access(editing)?;
            writable = editing;
         }
         let begun = Instant::now();
         phase(&attrs, &mut ctx, index, socket)?;
         timings.push(format!(
            "{} {:.1}s",
            step.name,
            begun.elapsed().as_secs_f64()
         ));
      }
   }
   set_phase("finish");
   let finishing = Instant::now();
   let finished = if tests {
      finish::tests()
   } else {
      finish::run(attrs.plan.separate)
   };
   timings.push(format!("finish {:.1}s", finishing.elapsed().as_secs_f64()));
   note("phases", &timings.join(", "));
   finished?;
   if cached && let Err(error) = output::store(&attrs) {
      note("output-cache", &format!("store failed with {error}"));
   }
   Ok(())
}

enum Held {
   Directory,
   Link(PathBuf),
   File(Vec<u8>, Permissions),
}

const PRIVATE_ETC: &str = "BIGTOP_PRIVATE_ETC";

/// Moves the build into its own namespaces with a copy of /etc, then re-execs
/// since /proc/self/exe no longer resolves there.
fn private_etc() -> io::Result<Infallible> {
   let exe = current_exe()?;
   let etc = Path::new("/etc");
   let mut entries = Vec::new();
   let mut pending = vec![PathBuf::new()];
   while let Some(relative) = pending.pop() {
      for listed in read_dir(etc.join(&relative))? {
         let entry = listed?;
         let name = relative.join(entry.file_name());
         let kind = entry.file_type()?;
         if kind.is_dir() {
            pending.push(name);
         } else if kind.is_symlink() {
            entries.push((name, Held::Link(read_link(entry.path())?)));
         } else {
            let held = Held::File(read(entry.path())?, entry.metadata()?.permissions());
            entries.push((name, held));
         }
      }
      if !relative.as_os_str().is_empty() {
         entries.push((relative, Held::Directory));
      }
   }
   entries.sort_by(|left, right| left.0.cmp(&right.0));

   let (uid, gid) = (getuid().as_raw(), getgid().as_raw());
   // SAFETY: without UnshareFlags::FILES every thread keeps the one descriptor
   // table
   unsafe { unshare_unsafe(UnshareFlags::NEWUSER | UnshareFlags::NEWNS) }?;
   write("/proc/self/setgroups", "deny")?;
   write("/proc/self/uid_map", format!("{uid} {uid} 1"))?;
   write("/proc/self/gid_map", format!("{gid} {gid} 1"))?;
   mount(
      "tmpfs",
      etc,
      "tmpfs",
      MountFlags::NOSUID | MountFlags::NODEV,
      c"mode=0755",
   )?;

   for (name, held) in entries {
      let path = etc.join(name);
      match held {
         Held::Directory => create_dir_all(&path)?,
         Held::Link(target) => symlink(target, &path)?,
         Held::File(bytes, permissions) => {
            write(&path, bytes)?;
            set_permissions(&path, permissions)?;
         },
      }
   }
   etc_access(false)?;
   let mut sets = capabilities(None)?;
   sets.inheritable |= CapabilitySet::SYS_ADMIN;
   set_capabilities(None, sets)?;
   configure_capability_in_ambient_set(CapabilitySet::SYS_ADMIN, true)?;
   Err(
      Command::new(exe)
         .args(args_os().skip(1))
         .env(PRIVATE_ETC, "1")
         .exec(),
   )
}

/// Whether the step is a test or a nu step just before one.
fn edits_etc(phases: &[Step], index: usize) -> bool {
   phases[index..]
      .iter()
      .find(|step| step.test || !matches!(step.kind, Kind::Nu { .. }))
      .is_some_and(|step| step.test)
}

/// Remounts the private /etc writable or read-only.
fn etc_access(writable: bool) -> io::Result<()> {
   let mut flags = MountFlags::NOSUID | MountFlags::NODEV;
   if !writable {
      flags |= MountFlags::RDONLY;
   }
   Ok(mount_remount("/etc", flags, c"")?)
}

pub fn setup(attrs: &Attrs, source: Source<'_>, socket: &Path) -> io::Result<Ctx> {
   set_phase("prepare");
   let bins = attrs
      .build_dependencies
      .iter()
      .map(|dep| format!("{dep}/bin"))
      .collect::<Vec<String>>();
   export("PATH", &bins.join(":"));
   let prepared = prepare(source)?;
   apply(&prepared.env);
   set_current_dir(&prepared.cwd)?;
   let ctx = prepared.ctx;
   for system in &attrs.plan.setups {
      set_phase(&format!("{system}.setup"));
      note("setup", system);
      dispatch::setup(system, &ctx, attrs, socket)?.apply();
   }
   set_current_dir(match attrs.plan.setups.first() {
      Some(system) => dispatch::workdir(system, &ctx)?,
      None => ctx.src.clone(),
   })?;
   Ok(ctx)
}

pub fn phase(attrs: &Attrs, ctx: &mut Ctx, index: usize, socket: &Path) -> io::Result<()> {
   let step = &attrs.plan.phases[index];
   set_phase(&step.name);
   let workdir = match attrs.plan.setups.first() {
      Some(system) => dispatch::workdir(system, ctx)?,
      None => ctx.src.clone(),
   };
   if step.test
      && ctx.cache
      && let Ok(client) = Client::connect(socket)
   {
      return tested(attrs, ctx, index, &workdir, client);
   }
   match step.kind {
      Kind::System {
         ref system,
         ref verb,
      } => dispatch::phase(system, verb, ctx, attrs, socket),
      Kind::Nu { ref body } => {
         set_current_dir(&workdir)?;
         let top = build_top()?;
         nu_phase(attrs, index, &step.name, body, &top)?;
         *ctx = Ctx::load(&top)?;
         Ok(())
      },
   }
}

/// A test step in a child process, which the test cache traces, nu's
/// environment taken back only when it ran.
fn tested(
   attrs: &Attrs,
   ctx: &mut Ctx,
   index: usize,
   workdir: &Path,
   client: Client,
) -> io::Result<()> {
   let step = &attrs.plan.phases[index];
   let top = build_top()?;
   match step.kind {
      Kind::System {
         ref system,
         ref verb,
      } => {
         let phase = |action: &str| {
            let mut command = Command::new(current_exe()?);
            command.args(["system", "phase", system, action]);
            io::Result::Ok(command)
         };
         let tests = dispatch::tests(system)?;
         if tests.is_some_and(Tests::prepares) {
            let status = phase("test-prepare")?.status()?;
            if !status.success() {
               return Err(io::Error::other(format!(
                  "phase {} failed building its tests, {status}",
                  step.name
               )));
            }
         }
         let module = dispatch::module(system)?;
         test_cache::run(client, ctx, step, Some(module), tests, &|| phase(verb))?;
      },
      Kind::Nu { ref body } => {
         let command = || {
            let mut command = nu_command(attrs, index, body, &top)?;
            command.current_dir(workdir);
            Ok(command)
         };
         if matches!(
            test_cache::run(client, ctx, step, None, None, &command)?,
            Ran::Passed
         ) {
            handover(&top)?;
            *ctx = Ctx::load(&top)?;
         }
      },
   }
   Ok(())
}
