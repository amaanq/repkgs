use std::{
   env::{
      args_os,
      var,
      var_os,
   },
   ffi::{
      OsStr,
      OsString,
   },
   fmt::Arguments,
   fs::OpenOptions,
   io::{
      self,
      Write as _,
   },
   os::unix::process::ExitStatusExt as _,
   path::{
      Path,
      PathBuf,
   },
   process::{
      Command,
      ExitCode,
      ExitStatus,
      Stdio,
   },
   time::Instant,
};

use eyre::{
   Result,
   eyre,
};
use pound::Error as ArgumentError;
use wire::client::Client;

pub struct Slot<'client> {
   client: &'client mut Client,
   build:  String,
}

impl Slot<'_> {
   #[inline]
   pub fn acquire(client: &mut Client) -> io::Result<Option<Slot<'_>>> {
      if var("JIG_SLOT").is_ok_and(|value| !value.is_empty()) {
         return Ok(None);
      }
      let build = var("NIX_BUILD_TOP").unwrap_or_else(|_| "-".to_owned());
      client.acquire(&build)?;
      Ok(Some(Slot { client, build }))
   }
}

impl Drop for Slot<'_> {
   #[inline]
   fn drop(&mut self) {
      if let Err(error) = self.client.release(&self.build) {
         diagnostic(format_args!("jig could not release compiler slot, {error}"));
      }
   }
}

pub struct RunResult {
   pub status: i32,
   pub stderr: Vec<u8>,
}

#[inline]
pub fn diagnostic(message: Arguments<'_>) {
   drop(writeln!(io::stderr().lock(), "{message}"));
}

#[inline]
pub fn log(tool: &str, outcome: &str, subject: &str, started: Instant) {
   if let Ok(path) = var("JIG_LOG")
      && !path.is_empty()
   {
      let subject_text = subject.replace(['\t', '\r', '\n'], " ");
      let record = format!(
         "{tool}\t{outcome}\t{subject_text}\t{}\n",
         started.elapsed().as_millis()
      );
      let result = OpenOptions::new()
         .create(true)
         .append(true)
         .open(path)
         .and_then(|mut file| file.write_all(record.as_bytes()));
      if let Err(error) = result {
         diagnostic(format_args!(
            "jig could not write its invocation log, {error}"
         ));
      }
   }
}

#[inline]
pub fn log_uncached(outcome: &str, args: &[String]) {
   if outcome == "query" {
      return;
   }
   if let Ok(path) = var("JIG_LOG_ARGS")
      && !path.is_empty()
   {
      let record = format!("{outcome}\t{}\n", args.join(" "));
      let result = OpenOptions::new()
         .create(true)
         .append(true)
         .open(path)
         .and_then(|mut file| file.write_all(record.as_bytes()));
      if let Err(error) = result {
         diagnostic(format_args!(
            "jig could not write its uncached log, {error}"
         ));
      }
   }
}

impl RunResult {
   #[must_use]
   #[inline]
   pub fn deterministic(&self) -> bool {
      self.status < 128
         && !String::from_utf8_lossy(&self.stderr).contains("unable to execute command")
         && !String::from_utf8_lossy(&self.stderr).contains("PLEASE submit a bug report")
   }
}

#[must_use]
#[inline]
pub fn status_code(status: ExitStatus) -> i32 {
   status
      .code()
      .unwrap_or_else(|| 128_i32 + status.signal().unwrap_or(1_i32))
}

#[inline]
pub fn run_slotted<Argument>(
   program: &OsStr,
   args: &[Argument],
   socket: Option<&Path>,
   capture: bool,
) -> io::Result<RunResult>
where
   Argument: AsRef<OsStr>,
{
   let mut client = socket.and_then(|path| Client::connect(path).ok());
   let slot = client
      .as_mut()
      .and_then(|connected| Slot::acquire(connected).ok().flatten());
   run(program, args, slot, capture)
}

#[inline]
pub fn run<Argument>(
   program: &OsStr,
   args: &[Argument],
   slot: Option<Slot<'_>>,
   capture: bool,
) -> io::Result<RunResult>
where
   Argument: AsRef<OsStr>,
{
   let mut command = Command::new(program);
   command
      .args(args)
      .stdin(Stdio::inherit())
      .stdout(Stdio::inherit());
   if slot.is_some() {
      command.env("JIG_SLOT", "1");
   }
   let result = if capture {
      command.stderr(Stdio::piped()).output().map(|output| {
         RunResult {
            status: status_code(output.status),
            stderr: output.stderr,
         }
      })
   } else {
      command.status().map(|status| {
         RunResult {
            status: status_code(status),
            stderr: Vec::new(),
         }
      })
   };
   drop(slot);
   match result {
      Err(error) if error.kind() == io::ErrorKind::NotFound => {
         let message = format!("jig could not start {}, {error}", program.to_string_lossy());
         if !capture {
            diagnostic(format_args!("{message}"));
         }
         Ok(RunResult {
            status: 127,
            stderr: format!("{message}\n").into_bytes(),
         })
      },
      other => other,
   }
}

#[inline]
pub fn socket() -> PathBuf {
   var_os("JIG_SOCK").map_or_else(
      || {
         let store = Path::new(option_env!("JIG_STORE_DIR").unwrap_or("/nix/store"));
         store
            .parent()
            .unwrap_or_else(|| Path::new("/nix"))
            .join("var/nix/jigd/socket")
      },
      PathBuf::from,
   )
}

#[inline]
pub fn text_arguments(args: &[OsString]) -> Result<Vec<String>> {
   args
      .iter()
      .map(|arg| {
         arg.to_str()
            .map(str::to_owned)
            .ok_or_else(|| eyre!("argument {arg:?} is not UTF-8"))
      })
      .collect()
}

#[inline]
pub fn run_main<Dispatch>(name: &str, dispatch: Dispatch) -> ExitCode
where
   Dispatch: FnOnce() -> Result<i32>,
{
   if let Err(error) = color_eyre::install() {
      diagnostic(format_args!(
         "{name} could not install error reporting, {error}"
      ));
      return ExitCode::FAILURE;
   }
   let asked = args_os().any(|arg| arg == "-h" || arg == "--help" || arg == "--version");
   let result = dispatch().or_else(|error| {
      match error.downcast_ref::<ArgumentError>() {
         Some(parsed) if parsed.is_exit() && asked => {
            writeln!(io::stdout().lock(), "{parsed}")?;
            Ok(0_i32)
         },
         Some(parsed) if parsed.is_exit() => {
            diagnostic(format_args!("{parsed}"));
            Ok(1_i32)
         },
         _ => Err(error),
      }
   });
   match result {
      Ok(status) => ExitCode::from(u8::try_from(status).unwrap_or(1)),
      Err(error) => {
         diagnostic(format_args!("{error:#}"));
         ExitCode::FAILURE
      },
   }
}
