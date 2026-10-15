use std::{
   collections::BTreeMap,
   convert::Infallible,
   env::{
      VarError,
      args_os,
      current_dir,
      current_exe,
      set_current_dir,
      var,
      var_os,
      vars,
   },
   ffi::OsStr,
   fs::{
      File,
      OpenOptions,
      Permissions,
      create_dir_all,
      metadata,
      read,
      read_dir,
      read_link,
      read_to_string,
      remove_file,
      set_permissions,
      symlink_metadata,
      write,
   },
   io::{
      self,
      ErrorKind,
      Read as _,
   },
   os::{
      fd::AsRawFd as _,
      unix::{
         ffi::OsStrExt as _,
         fs::{
            OpenOptionsExt as _,
            PermissionsExt as _,
            symlink,
         },
         process::CommandExt as _,
      },
   },
   path::{
      Path,
      PathBuf,
   },
   process::Command,
   thread,
   time::Instant,
};

use mummer::{
   files::write_file,
   process::Checked as _,
   tree::{
      note,
      set_phase,
      shebangs,
   },
};
use rustix::{
   io::Errno,
   mount::{
      MountFlags,
      mount,
      mount_bind,
      mount_bind_recursive,
      mount_remount,
   },
   process::{
      chroot,
      getgid,
      getuid,
      ioctl_tiocsctty,
      setsid,
   },
   pty::{
      OpenptFlags,
      grantpt,
      openpt,
      ptsname,
      unlockpt,
   },
   stdio::dup2_stdin,
   termios::{
      Winsize,
      tcsetwinsize,
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
      Role,
      TestVar,
      build_top,
   },
   finish,
   output,
   prepare::{
      Source,
      prepare,
   },
   progress,
   registry,
   ring::Announced,
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
   let etc = spec.tests.etc == Some(true);
   let fhs = spec.tests.fhs == Some(true);
   if etc || fhs {
      if var_os(PRIVATE).is_none() {
         match private(etc, fhs)? {}
      }
      let mut cleared = Env::default();
      cleared.unset(PRIVATE);
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
   let serving = registry::start(spec.tests.registry.as_ref(), &mut ctx.test_env)?;
   let mut writable = false;
   let mut furnished = false;
   let mut controlled = false;
   let mut undo = None;
   let mut resolved = false;
   for (index, step) in attrs.plan.phases.iter().enumerate() {
      let testing = step.role != Role::Build;
      let wanted = if tests {
         testing
      } else {
         !testing || (spec.tests.run.unwrap_or(true) && !attrs.plan.separate && ctx.tests_run)
      };
      if wanted {
         let editing = etc && testing;
         if editing != writable {
            etc_access(editing)?;
            writable = editing;
         }
         match (testing, undo.take()) {
            (true, None) => {
               undo = Some(overlay(&ctx.test_env)?);
               // test tools like perl only reach PATH here
               if !resolved {
                  shebangs(&ctx.src, false)?;
                  resolved = true;
               }
            },
            (false, Some(previous)) => previous.apply(),
            (_, kept) => undo = kept,
         }
         if fhs && testing != furnished {
            furnish(testing)?;
            furnished = testing;
         }
         if spec.tests.terminal == Some(true) && testing && !controlled {
            take_terminal()?;
            controlled = true;
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
   serving.map(registry::Serving::stop).transpose()?;
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

const PRIVATE: &str = "BIGTOP_PRIVATE";

/// Moves the build into its own namespaces, with a copy of /etc for `etc` and
/// a root of its own for `fhs`, then re-execs since /proc/self/exe no longer
/// resolves there.
fn private(etc: bool, fhs: bool) -> io::Result<Infallible> {
   let exe = current_exe()?;
   let entries = if etc { held_etc()? } else { Vec::new() };
   let cwd = current_dir()?;

   let (uid, gid) = (getuid().as_raw(), getgid().as_raw());
   // SAFETY: without UnshareFlags::FILES every thread keeps the one descriptor
   // table
   unsafe { unshare_unsafe(UnshareFlags::NEWUSER | UnshareFlags::NEWNS) }?;
   write("/proc/self/setgroups", "deny")?;
   write("/proc/self/uid_map", format!("{uid} {uid} 1"))?;
   write("/proc/self/gid_map", format!("{gid} {gid} 1"))?;
   if fhs {
      private_root()?;
      set_current_dir(&cwd)?;
   }
   if etc {
      private_etc(entries)?;
   }
   let mut sets = capabilities(None)?;
   sets.inheritable |= CapabilitySet::SYS_ADMIN;
   set_capabilities(None, sets)?;
   configure_capability_in_ambient_set(CapabilitySet::SYS_ADMIN, true)?;
   Err(
      Command::new(exe)
         .args(args_os().skip(1))
         .env(PRIVATE, "1")
         .exec(),
   )
}

/// /etc as it is, to copy into the private one.
fn held_etc() -> io::Result<Vec<(PathBuf, Held)>> {
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
   Ok(entries)
}

fn private_etc(entries: Vec<(PathBuf, Held)>) -> io::Result<()> {
   let etc = Path::new("/etc");
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
   etc_access(false)
}

/// The sandbox's root is not writable, so a tmpfs over /bin becomes the root,
/// every other top-level entry bound into it. The sandbox has no /usr, so the
/// new root gets an empty one for `furnish` to fill.
fn private_root() -> io::Result<()> {
   let shell = OpenOptions::new()
      .read(true)
      .custom_flags(libc::O_PATH)
      .open("/bin/sh")?;
   let listed = read_dir("/")?
      .map(|listing| {
         let entry = listing?;
         Ok((entry.file_name(), entry.file_type()?))
      })
      .collect::<io::Result<Vec<_>>>()?;
   let root = Path::new("/bin");
   mount(
      "tmpfs",
      root,
      "tmpfs",
      MountFlags::NOSUID | MountFlags::NODEV,
      c"mode=0755",
   )?;
   for (name, kind) in listed {
      if name == "bin" || name == "usr" {
         continue;
      }
      let original = Path::new("/").join(&name);
      let placed = root.join(&name);
      if kind.is_symlink() {
         symlink(read_link(&original)?, &placed)?;
         continue;
      }
      if kind.is_dir() {
         create_dir_all(&placed)?;
      } else {
         write(&placed, [])?;
      }
      mount_bind_recursive(&original, &placed)?;
   }
   create_dir_all(root.join("usr/bin"))?;
   create_dir_all(root.join("bin"))?;
   let sh = root.join("bin/sh");
   write(&sh, [])?;
   mount_bind(format!("/proc/self/fd/{}", shell.as_raw_fd()), &sh)?;
   chroot(root)?;
   Ok(())
}

/// Links every program on PATH into /bin and /usr/bin, for tests that name
/// them there, or takes the links away again.
fn furnish(on: bool) -> io::Result<()> {
   const BINS: [&str; 2] = ["/bin", "/usr/bin"];
   for bin in BINS {
      for entry in read_dir(bin)? {
         let path = entry?.path();
         if symlink_metadata(&path)?.is_symlink() {
            remove_file(&path)?;
         }
      }
   }
   if !on {
      return Ok(());
   }
   let path = var("PATH").map_err(|error| io::Error::other(format!("PATH: {error}")))?;
   for dir in path.split(':').filter(|dir| !dir.is_empty()) {
      let listed = match read_dir(dir) {
         Ok(listed) => listed,
         Err(error) if error.kind() == ErrorKind::NotFound => continue,
         Err(error) => return Err(error),
      };
      for listing in listed {
         let entry = listing?;
         let program = entry.path();
         let runs = metadata(&program)
            .is_ok_and(|found| found.is_file() && found.permissions().mode() & 0o111 != 0);
         if !runs {
            continue;
         }
         for bin in BINS {
            let link = Path::new(bin).join(entry.file_name());
            if symlink_metadata(&link).is_err() {
               symlink(&program, &link)?;
            }
         }
      }
   }
   Ok(())
}

/// Makes a pseudo-terminal the session's controlling terminal and stdin, for
/// tests that open /dev/tty, check `test -t 0` or start shells with job
/// control. Output to it is drained and dropped, and nothing is ever typed, so
/// a read from it never returns. Under the pid 1 reaper the build leads no
/// session, and only a leader can take a terminal.
fn take_terminal() -> io::Result<()> {
   match setsid() {
      Ok(_) | Err(Errno::PERM) => {},
      Err(error) => return Err(error.into()),
   }
   let master = openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY | OpenptFlags::CLOEXEC)?;
   grantpt(&master)?;
   unlockpt(&master)?;
   let name = ptsname(&master, Vec::new())?;
   let terminal = OpenOptions::new()
      .read(true)
      .write(true)
      .custom_flags(libc::O_NOCTTY | libc::O_CLOEXEC)
      .open(OsStr::from_bytes(name.to_bytes()))?;
   tcsetwinsize(&terminal, Winsize {
      ws_row:    24,
      ws_col:    80,
      ws_xpixel: 0,
      ws_ypixel: 0,
   })?;
   ioctl_tiocsctty(&terminal).map_err(|error| {
      io::Error::other(format!(
         "a terminal needs bigtop to lead a session without one: {error}"
      ))
   })?;
   dup2_stdin(&terminal)?;
   let mut reader = File::from(master);
   thread::spawn(move || {
      let _held = terminal;
      let mut buffer = [0; 4096];
      while reader.read(&mut buffer).is_ok_and(|read| read > 0) {}
   });
   Ok(())
}

/// Puts `env` over this process's environment and returns what undoes it.
fn overlay(env: &BTreeMap<String, TestVar>) -> io::Result<Env> {
   let mut previous = Env::default();
   for (name, change) in env {
      let (current, present) = match var(name) {
         Ok(value) => (value, true),
         Err(VarError::NotPresent) => (String::new(), false),
         Err(VarError::NotUnicode(_raw)) => {
            return Err(io::Error::other(format!("{name} is not UTF-8")));
         },
      };
      match *change {
         TestVar::Set(ref value) => export(name, value),
         TestVar::Prepend {
            ref separator,
            ref entries,
         } => {
            let kept = current
               .split(separator.as_str())
               .filter(|entry| !entry.is_empty());
            let joined = entries
               .iter()
               .map(String::as_str)
               .chain(kept)
               .collect::<Vec<_>>()
               .join(separator);
            export(name, &joined);
         },
      }
      if present {
         previous.set(name, current);
      } else {
         previous.unset(name);
      }
   }
   Ok(previous)
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
   progress::phase(&step.name, || {
      let workdir = match attrs.plan.setups.first() {
         Some(system) => dispatch::workdir(system, ctx)?,
         None => ctx.src.clone(),
      };

      if step.role == Role::Test
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
   })
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
            test_cache::run(client, ctx, step, None, Some(&Announced), &command)?,
            Ran::Passed
         ) {
            handover(&top)?;
            *ctx = Ctx::load(&top)?;
         }
      },
   }
   Ok(())
}
