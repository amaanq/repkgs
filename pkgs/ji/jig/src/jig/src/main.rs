mod bootstrap;
mod cc;
mod cli;
mod driver;
mod gocache;
mod header;
mod manifest;
mod nix_store;
mod rustc;

use std::{
   env::args_os,
   ffi::OsString,
   fs::read,
   path::Path,
   process::ExitCode,
};

use eyre::{
   Result,
   bail,
   eyre,
};
use jig::{
   directory::{
      get_directory,
      put_directory,
   },
   files::write_file,
   fixup,
   identify,
   process::{
      self,
      diagnostic,
      run_main,
      socket,
      text_arguments,
   },
};
use wire::client::Client;

use crate::cli::{
   CacheOperation,
   Mode,
};

fn dispatch() -> Result<i32> {
   let arguments = args_os().collect::<Vec<_>>();
   let executable = arguments
      .first()
      .and_then(|name| Path::new(name).file_name())
      .unwrap_or_default();
   let mut args = &arguments[1..];
   let name = if executable == "jig" {
      let Some((command, rest)) = args.split_first() else {
         bail!("jig requires a mode")
      };
      args = rest;
      command.as_os_str()
   } else {
      executable
   };
   let socket = socket();
   let name_text = name
      .to_str()
      .ok_or_else(|| eyre!("mode {name:?} is not UTF-8"))?;
   run(
      Mode::from_arguments(name_text, args)?,
      name_text,
      args,
      &socket,
   )
}

fn slot_mode(args: &[OsString], socket: &Path) -> Result<i32> {
   let Some((program, rest)) = args.split_first() else {
      bail!("jig slot requires a program")
   };
   Ok(process::run_slotted(program, rest, Some(socket), false)?.status)
}

fn run(mode: Mode, name: &str, args: &[OsString], socket: &Path) -> Result<i32> {
   match mode {
      Mode::Cc | Mode::Cxx => return Ok(cc::run_cc(name, &text_arguments(args)?, socket)?),
      Mode::Rustcwrap => return Ok(rustc::run_rustc(&text_arguments(args)?, socket)?),
      Mode::Cache {
         operation,
         key,
         path,
      } => return cache_mode(operation, &key, &path, socket),
      Mode::Slot => return slot_mode(args, socket),
      Mode::Gocacheprog => gocache::run(socket)?,
      Mode::HeaderKey => return header::lookup(&text_arguments(args)?, socket),
      Mode::RelocFixup {
         directory,
         dest,
         deny,
      } => fixup::run(&directory, dest.as_deref(), &deny)?,
      Mode::NixStore { operation } => nix_store::run(&operation)?,
      Mode::Identify { roots } => identify::run(&roots, socket)?,
      Mode::Bootstrap { recipe } => bootstrap::dispatch::mode(&recipe)?,
   }
   Ok(0)
}

fn cache_mode(operation: CacheOperation, key: &str, file: &Path, socket: &Path) -> Result<i32> {
   let mut client = match Client::connect(socket) {
      Ok(client) => client,
      Err(error) => {
         diagnostic(format_args!("jig cache unavailable, {error}"));
         return Ok(2);
      },
   };
   match operation {
      CacheOperation::Get => {
         let Some(bytes) = client.get(key)? else {
            return Ok(1);
         };
         write_file(file, &bytes, None)?;
      },
      CacheOperation::Put => client.put(key, &read(file)?)?,
      CacheOperation::GetDir => {
         return Ok(i32::from(!get_directory(&mut client, key, file)?));
      },
      CacheOperation::PutDir => put_directory(&mut client, key, file)?,
   }
   Ok(0)
}

fn main() -> ExitCode {
   run_main("jig", dispatch)
}
