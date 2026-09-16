mod attrs;
mod build;
mod bundle;
mod cli;
mod debug;
mod dev;
mod env;
mod exports;
mod finish;
mod hardening;
mod implant;
mod launchers;
mod prepare;
mod probe;
mod site;
mod system;

use std::{
   collections::BTreeMap,
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
   syslibs::{
      Tables,
      load,
      wanted,
      wanted_for,
   },
   tree,
};
use pound::Parse as _;

use crate::{
   attrs::{
      Attrs,
      Ctx,
      build_top,
   },
   cli::{
      Mode,
      ProbeOperation,
      SyslibsOperation,
   },
   system::{
      dispatch,
      msbuild,
      syslibs,
   },
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

fn lock_tables(path: Option<&Path>) -> io::Result<Tables> {
   path.map_or_else(|| syslibs::tables(&Attrs::load()?), load)
}

fn syslibs_mode(operation: SyslibsOperation) -> io::Result<()> {
   let found = match operation {
      SyslibsOperation::Wanted { dir, tables } => wanted(&lock_tables(tables.as_deref())?, &dir)?,
      SyslibsOperation::WantedFor {
         ecosystem,
         names,
         tables,
      } => wanted_for(&lock_tables(tables.as_deref())?, &ecosystem, &names),
   };
   let mut stdout = io::stdout().lock();
   serde_json::to_writer(&mut stdout, &found)?;
   writeln!(stdout)?;
   Ok(())
}

fn run(mode: Mode, socket: &Path) -> Result<i32> {
   match mode {
      Mode::Build { tests, from_tree } => build::run(tests, from_tree.as_deref(), socket)?,
      Mode::Dev { operation } => dev::run(operation, socket)?,
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
      Mode::Msbuild { file } => msbuild::run(&file)?,
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
      Mode::Launcher {
         name,
         program,
         args,
         env,
      } => {
         let vars = env
            .as_deref()
            .map(serde_json::from_str::<BTreeMap<String, String>>)
            .transpose()?
            .unwrap_or_default();
         let ctx = Ctx::load(&build_top()?)?;
         launchers::write(&ctx, &name, &program, &args, &vars)?;
      },
      Mode::Bundle => bundle::run()?,
      Mode::Syslibs { operation } => syslibs_mode(operation)?,
      Mode::Schema { check } => hardening::schema(check.as_deref())?,
   }
   Ok(0)
}

fn main() -> ExitCode {
   run_main("jib", dispatch)
}
