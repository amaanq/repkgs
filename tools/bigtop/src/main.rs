#[macro_use]
mod schema;

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
mod output;
mod prepare;
mod probe;
mod recipe;
mod site;
mod system;
mod targets;

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

use misstep::Result;
use mummer::{
   process::{
      run_main,
      socket,
      text_arguments,
   },
   syslibs::{
      CargoBuild,
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
   launchers::entries::{
      run as launch_outputs,
      write as write_launcher,
   },
   system::{
      cabal::ordered_ghc_pkg,
      dispatch,
      msbuild,
   },
};

fn dispatch() -> Result<i32> {
   let arguments = args_os().collect::<Vec<_>>();
   let text = text_arguments(arguments.get(1..).unwrap_or_default())?;
   if arguments
      .first()
      .and_then(|program| Path::new(program).file_name())
      .is_some_and(|name| name == "ghc-pkg")
   {
      return Ok(ordered_ghc_pkg(&text)?);
   }
   let mode = Mode::try_parse_from(text.iter().map(String::as_str))?;
   run(mode, &socket())
}

fn probe_mode(operation: ProbeOperation, socket: &Path) -> Result<i32> {
   match operation {
      ProbeOperation::Key {
         kind,
         scripts,
         extra,
      } => {
         let parsed = extra
            .as_deref()
            .map(serde_json::from_str::<serde_json::Value>)
            .transpose()?;
         let key = probe::key(&kind, &scripts, parsed.as_ref())?;
         writeln!(io::stdout().lock(), "{key}")?;
      },
      ProbeOperation::Restore { key, file } => {
         return Ok(i32::from(!probe::restore(&key.parse()?, &file, socket)?));
      },
      ProbeOperation::Store { key, file } => probe::store(&key.parse()?, &file, socket)?,
      ProbeOperation::RestoreDir { key, dir } => {
         return Ok(i32::from(!probe::restore_dir(&key.parse()?, &dir, socket)?));
      },
      ProbeOperation::StoreDir { key, dir } => probe::store_dir(&key.parse()?, &dir, socket)?,
      ProbeOperation::CmakeInit { cache } => {
         writeln!(io::stdout().lock(), "{}", probe::cmake_init(&cache)?)?;
      },
   }
   Ok(0)
}

fn lock_tables(path: Option<&Path>) -> io::Result<Tables> {
   path.map_or_else(|| Ok(Attrs::load()?.sys_libs), load)
}

fn syslibs_mode(operation: SyslibsOperation) -> io::Result<()> {
   let found = match operation {
      SyslibsOperation::Wanted { dir, tables } => {
         wanted(
            &lock_tables(tables.as_deref())?,
            &dir,
            Some(&CargoBuild::default()),
         )?
      },
      SyslibsOperation::WantedFor {
         ecosystem,
         names,
         tables,
      } => wanted_for(&lock_tables(tables.as_deref())?, ecosystem.parse()?, &names),
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
      Mode::Relativize { prefix, dest } => tree::relativize(&prefix, &dest)?,
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
      } => launch_outputs(&output, &launch, &interp, ldso, &deps)?,
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
         write_launcher(&ctx, &name, &program, &args, &vars)?;
      },
      Mode::Bundle => bundle::run()?,
      Mode::Syslibs { operation } => syslibs_mode(operation)?,
      Mode::Schema { check } => schema::run(check.as_deref())?,
   }
   Ok(0)
}

fn main() -> ExitCode {
   run_main(dispatch)
}
