use std::{
   env::var,
   ffi::OsStr,
   fmt::Arguments,
   fs::OpenOptions,
   io::{
      self,
      Write,
   },
   os::unix::process::ExitStatusExt,
   path::Path,
   process::{
      Command,
      ExitStatus,
      Stdio,
   },
   time::Instant,
};

use wire::client::Client;

pub struct Slot<'client> {
   client: &'client mut Client,
   build:  String,
}

impl Slot<'_> {
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

pub fn diagnostic(message: Arguments<'_>) {
   let _ = writeln!(io::stderr().lock(), "{message}");
}

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

pub fn status_code(status: ExitStatus) -> i32 {
   status
      .code()
      .unwrap_or_else(|| 128 + status.signal().unwrap_or(1))
}

pub fn run_slotted<Argument: AsRef<OsStr>>(
   program: &OsStr,
   args: &[Argument],
   socket: Option<&Path>,
   capture: bool,
) -> io::Result<RunResult> {
   let mut client = socket.and_then(|path| Client::connect(path).ok());
   let slot = client
      .as_mut()
      .and_then(|client| Slot::acquire(client).ok().flatten());
   run(program, args, slot, capture)
}

pub fn run<Argument: AsRef<OsStr>>(
   program: &OsStr,
   args: &[Argument],
   slot: Option<Slot<'_>>,
   capture: bool,
) -> io::Result<RunResult> {
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
