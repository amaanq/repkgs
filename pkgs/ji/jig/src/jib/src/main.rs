mod attrs;
mod cli;
mod debug;
mod env;
mod exports;
mod finish;
mod implant;
mod launchers;
mod prepare;
mod probe;
mod site;
mod system;

use std::{
   env::args_os,
   io::{
      self,
      Write as _,
   },
   path::Path,
   process::ExitCode,
};

use eyre::Result;
use jig::{
   process::{
      run_main,
      socket,
      text_arguments,
   },
   tree,
};
use pound::Parse as _;

use crate::{
   cli::{
      Mode,
      ProbeOperation,
   },
   system::dispatch,
};

fn dispatch() -> Result<i32> {
   let arguments = args_os().collect::<Vec<_>>();
   let text = text_arguments(arguments.get(1..).unwrap_or_default())?;
   let mode = Mode::try_parse_from(text.iter().map(String::as_str))?;
   run(mode, &socket())
}

fn probe_mode(operation: ProbeOperation, socket: &Path) -> Result<i32> {
   match operation {
      ProbeOperation::Key {
         kind,
         scripts,
         extra,
         no_out,
      } => {
         let parsed = extra
            .as_deref()
            .map(serde_json::from_str::<serde_json::Value>)
            .transpose()?;
         let key = probe::key(&kind, &scripts, parsed.as_ref(), no_out, socket)?;
         writeln!(io::stdout().lock(), "{key}")?;
      },
      ProbeOperation::Restore { key, file } => {
         return Ok(i32::from(!probe::restore(&key, &file, socket)?));
      },
      ProbeOperation::Store { key, file } => probe::store(&key, &file, socket)?,
      ProbeOperation::RestoreDir { key, dir } => {
         return Ok(i32::from(!probe::restore_dir(&key, &dir, socket)?));
      },
      ProbeOperation::StoreDir { key, dir } => probe::store_dir(&key, &dir, socket)?,
      ProbeOperation::CmakeInit { cache } => {
         writeln!(io::stdout().lock(), "{}", probe::cmake_init(&cache)?)?;
      },
   }
   Ok(0)
}

fn run(mode: Mode, socket: &Path) -> Result<i32> {
   match mode {
      Mode::Shebangs { directory, undo } => tree::shebangs(&directory, undo)?,
      Mode::Exports { roots } => exports::print(&roots, false)?,
      Mode::Deps { roots } => exports::print(&roots, true)?,
      Mode::WriteExports { output } => exports::write(&output)?,
      Mode::Prune { output } => tree::print_pruned(&output)?,
      Mode::Relativize {
         prefix,
         dest,
         macho,
      } => tree::relativize(&prefix, &dest, macho)?,
      Mode::SplitDebug { output, debug } => debug::run(&output, &debug)?,
      Mode::Prepare { from_tree } => prepare::run(from_tree.as_deref())?,
      Mode::Finish { keep_tree, tests } => finish::mode(keep_tree, tests)?,
      Mode::Probe { operation } => return probe_mode(operation, socket),
      Mode::System { operation } => dispatch::mode(operation, socket)?,
      Mode::Env { out, cache } => env::run(&out, cache)?,
      Mode::Implant {
         directory,
         interp,
         stub,
         deps,
      } => implant::run(&directory, &interp, stub.as_deref(), &deps)?,
      Mode::Launchers {
         output,
         launch,
         interp,
         ldso,
         deps,
      } => launchers::run(&output, &launch, &interp, ldso, &deps)?,
   }
   Ok(0)
}

fn main() -> ExitCode {
   run_main("jib", dispatch)
}
