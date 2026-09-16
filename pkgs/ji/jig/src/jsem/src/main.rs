//! `jsem <ghc> [args…]` execs ghc. The first jsem of a build also starts the
//! one broker that feeds cabal's `-jsem <name>` semaphore from jigd slots
//! until cabal removes it.

mod broker;
mod sem;

use std::{
   env::{
      args_os,
      current_exe,
      var,
   },
   ffi::{
      OsStr,
      OsString,
   },
   fs::{
      File,
      OpenOptions,
   },
   io,
   os::unix::{
      fs::OpenOptionsExt as _,
      net::UnixStream,
      process::CommandExt as _,
   },
   process::{
      Command,
      ExitCode,
      Stdio,
   },
   thread::sleep,
   time::Duration,
};

use eyre::{
   Result,
   bail,
};
use jig::process::{
   diagnostic,
   run_main,
   socket,
};
use rustix::{
   fs::{
      FlockOperation,
      flock,
   },
   io::Errno,
};

use crate::{
   broker::{
      Broker,
      Orders,
   },
   sem::Sem,
};

const TICK: Duration = Duration::from_millis(10);
/// Ticks between checks that the semaphore is still linked.
const ALIVE_EVERY: u32 = 50;

fn main() -> ExitCode {
   run_main("jsem", run)
}

fn run() -> Result<i32> {
   let args: Vec<OsString> = args_os().skip(1).collect();
   match *args.as_slice() {
      [ref flag, ref name] if flag == "--broker" => broke(&name.to_string_lossy()),
      [ref program, ref rest @ ..] => launch(program, rest),
      [] => bail!("usage: jsem <ghc> [args...]"),
   }
}

/// cabal passes one argument "-jsem <name>".
fn semaphore_name(args: &[OsString]) -> Option<String> {
   let mut found = args.iter().enumerate().filter_map(|(index, arg)| {
      let rest = arg.to_str()?.strip_prefix("-jsem")?.trim_start();
      Some((index, rest))
   });
   let (index, rest) = found.next()?;
   let name = if rest.is_empty() {
      args.get(index + 1)?.to_string_lossy().into_owned()
   } else {
      rest.to_owned()
   };
   Some(if name.starts_with('/') {
      name
   } else {
      format!("/{name}")
   })
}

/// One broker per (build, semaphore).
fn lock(name: &str) -> io::Result<Option<File>> {
   let dir = var("NIX_BUILD_TOP")
      .or_else(|_absent| var("TMPDIR"))
      .unwrap_or_else(|_absent| "/tmp".to_owned());
   let file = OpenOptions::new()
      .read(true)
      .write(true)
      .create(true)
      .truncate(false)
      .mode(0o600)
      .open(format!("{dir}/.jsem{}.lock", name.replace('/', "_")))?;
   match flock(&file, FlockOperation::NonBlockingLockExclusive) {
      Ok(()) => Ok(Some(file)),
      Err(Errno::WOULDBLOCK) => Ok(None),
      Err(error) => Err(error.into()),
   }
}

fn launch(program: &OsStr, args: &[OsString]) -> Result<i32> {
   if let Some(name) = semaphore_name(args)
      && UnixStream::connect(socket()).is_ok()
      && lock(&name)?.is_some()
   {
      Command::new(current_exe()?)
         .arg("--broker")
         .arg(&name)
         .stdin(Stdio::null())
         .stdout(Stdio::null())
         .process_group(0)
         .spawn()?;
   }
   let error = Command::new(program).args(args).exec();
   diagnostic(format_args!("jsem: exec {}: {error}", program.display()));
   Ok(127)
}

fn broke(name: &str) -> Result<i32> {
   let Some(_held) = lock(name)? else {
      return Ok(0);
   };
   let sem = match Sem::open(name) {
      Ok(sem) => sem,
      Err(error) => bail!("cannot open semaphore {name}: {error}"),
   };
   let build = var("NIX_BUILD_TOP").unwrap_or_else(|_absent| "-".to_owned());
   let mut broker = Broker::new(sem, Orders::new(socket(), build));
   for tick in 0_u32.. {
      if tick % ALIVE_EVERY == 0 && Sem::open(name).is_err() {
         break;
      }
      broker.tick();
      sleep(TICK);
   }
   drop(broker);
   Ok(0)
}
