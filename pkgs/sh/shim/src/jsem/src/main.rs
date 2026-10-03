//! `jsem <ghc> [args…]` runs ghc. When cabal handed it a `-jsem v2-<name>`
//! semaphore, ghc gets a private POSIX semaphore instead. Extra capabilities
//! consume Cabal tokens and shimd slots until the child releases them or exits.

mod proxy;
mod sem;

use std::{
   env::{
      args_os,
      var_os,
   },
   ffi::{
      OsStr,
      OsString,
   },
   io::{
      self,
      Write as _,
   },
   os::unix::process::CommandExt as _,
   path::PathBuf,
   process::{
      Command,
      ExitCode,
      Stdio,
   },
   str::from_utf8,
   thread::sleep,
   time::Duration,
};

use eyre::{
   Result,
   bail,
};
use rustix::{
   io::Errno,
   process::{
      Pid,
      Signal,
      kill_process_group,
   },
};
use shim::process::{
   diagnostic,
   expand_response_files,
   run_main,
   socket,
   status_code,
   text_arguments,
};

use crate::{
   proxy::Proxy,
   sem::Sem,
};

fn main() -> ExitCode {
   run_main("jsem", run)
}

fn run() -> Result<i32> {
   let args: Vec<OsString> = args_os().skip(1).collect();
   let [ref program, ref rest @ ..] = *args.as_slice() else {
      bail!("usage: jsem <ghc> [args...]");
   };
   if rest.iter().any(|arg| arg == "--info") {
      return info(program, rest);
   }

   let expanded = expand_response_files(&text_arguments(rest)?)?;
   let Some(handed) = semaphore_name(&expanded) else {
      return Ok(exec(program, rest));
   };
   let directory = var_os("TMPDIR")
      .map_or_else(|| PathBuf::from("/tmp"), PathBuf::from)
      .join("semaphore-compat");
   let build = var_os("NIX_BUILD_TOP")
      .map_or_else(|| "-".to_owned(), |top| top.to_string_lossy().into_owned());
   let semaphore = Sem::new()?;
   let mut proxy = Proxy::new(&semaphore, directory.join(handed), socket(), build)?;
   let mut command = Command::new(program);

   command
      .args(rest)
      .args(["-RTS", "-jsem"])
      .arg(semaphore.name())
      .env("SHIM_SLOT", "1")
      .process_group(0);

   diagnostic(format_args!("jsem: {command:?}"));
   let mut child = command.spawn()?;
   let result = (|| -> Result<i32> {
      loop {
         if let Some(status) = child.try_wait()? {
            return Ok(status_code(status));
         }
         proxy.tick()?;
         sleep(Duration::from_millis(10));
      }
   })();

   if child.try_wait()?.is_none() {
      if let Err(error) = kill_process_group(Pid::from_child(&child), Signal::KILL)
         && error != Errno::SRCH
      {
         return Err(error.into());
      }

      child.wait()?;
   }

   result
}

fn info(program: &OsStr, args: &[OsString]) -> Result<i32> {
   let output = Command::new(program)
      .args(args)
      .stderr(Stdio::inherit())
      .output()?;

   if !output.status.success() {
      io::stdout().write_all(&output.stdout)?;
      return Ok(status_code(output.status));
   }

   let text = from_utf8(&output.stdout)?;
   let Some(properties) = text.trim_start().strip_prefix('[') else {
      bail!("ghc --info did not return a property list");
   };

   write!(io::stdout(), "[(\"Semaphore version\",\"2\"),{properties}")?;
   Ok(0)
}

fn exec(program: &OsStr, args: &[OsString]) -> i32 {
   let error = Command::new(program).args(args).exec();
   diagnostic(format_args!("jsem: exec {}: {error}", program.display()));
   127
}

/// Cabal writes `-jsem v2-<name>` as one response-file argument.
fn semaphore_name(args: &[String]) -> Option<&str> {
   let (index, rest) = args.iter().enumerate().find_map(|(index, arg)| {
      let rest = arg.strip_prefix("-jsem")?;
      Some((index, rest))
   })?;
   let identifier = if rest.trim_start().is_empty() {
      args.get(index + 1)?.as_str()
   } else {
      rest.trim_start()
   };
   identifier.strip_prefix("v2-")
}
