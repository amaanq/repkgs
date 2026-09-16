//! `jsem <ghc> [args…]` runs ghc. When cabal handed it a `-jsem v2-<name>`
//! semaphore and jigd is up, ghc gets a proxy semaphore instead, so every
//! extra capability it takes is a cabal token or a jigd slot.

mod proxy;

use std::{
   env::{
      args_os,
      var_os,
   },
   ffi::{
      OsStr,
      OsString,
   },
   os::unix::{
      net::UnixStream,
      process::CommandExt as _,
   },
   path::PathBuf,
   process::{
      Command,
      ExitCode,
      id,
   },
};

use eyre::{
   Result,
   bail,
};
use jig::process::{
   diagnostic,
   run_main,
   socket,
   status_code,
};

use crate::proxy::Proxy;

fn main() -> ExitCode {
   run_main("jsem", run)
}

fn run() -> Result<i32> {
   let args: Vec<OsString> = args_os().skip(1).collect();
   let [ref program, ref rest @ ..] = *args.as_slice() else {
      bail!("usage: jsem <ghc> [args...]");
   };
   let Some(handed) = Semaphore::find(rest).filter(|_found| UnixStream::connect(socket()).is_ok())
   else {
      return Ok(exec(program, rest));
   };
   let directory = var_os("TMPDIR")
      .map_or_else(|| PathBuf::from("/tmp"), PathBuf::from)
      .join("semaphore-compat");
   let proxied = format!("{}-j{}", handed.name, id());
   let build = var_os("NIX_BUILD_TOP")
      .map_or_else(|| "-".to_owned(), |top| top.to_string_lossy().into_owned());
   let proxy = Proxy::bind(
      directory.join(&proxied),
      directory.join(&handed.name),
      socket(),
      build,
   )?;
   let mut rewritten = rest.to_vec();
   rewritten[handed.index] = OsString::from(format!("{}v2-{proxied}", handed.prefix));
   let status = Command::new(program).args(&rewritten).status()?;
   drop(proxy);
   Ok(status_code(status))
}

fn exec(program: &OsStr, args: &[OsString]) -> i32 {
   let error = Command::new(program).args(args).exec();
   diagnostic(format_args!("jsem: exec {}: {error}", program.display()));
   127
}

/// Where cabal's `-jsem v2-<name>` sits in ghc's arguments, either as one
/// argument or as the one after the flag.
struct Semaphore {
   index:  usize,
   prefix: String,
   name:   String,
}

impl Semaphore {
   fn find(args: &[OsString]) -> Option<Self> {
      let (index, rest) = args.iter().enumerate().find_map(|(index, arg)| {
         let rest = arg.to_str()?.strip_prefix("-jsem")?;
         Some((index, rest))
      })?;
      let (position, prefix, identifier) = if rest.trim_start().is_empty() {
         (index + 1, String::new(), args.get(index + 1)?.to_str()?)
      } else {
         (
            index,
            format!("-jsem{}", &rest[..rest.len() - rest.trim_start().len()]),
            rest.trim_start(),
         )
      };
      let name = identifier.strip_prefix("v2-")?;
      Some(Self {
         index: position,
         prefix,
         name: name.to_owned(),
      })
   }
}
