mod bootstrap;
mod cc;
mod cli;
mod driver;
mod fetch;
mod ghc;
mod gocache;
mod header;
mod manifest;
mod nix_store;
mod record;
mod rustc;
mod tblgen;
mod wrapper;

use std::{
   env::args_os,
   ffi::OsString,
   fs::read,
   path::Path,
   process::ExitCode,
};

use misstep::{
   Result,
   bail,
   report,
};
use mummer::{
   directory::{
      get_directory,
      put_directory,
   },
   files::write_file,
   fixup,
   process::{
      self,
      diagnostic,
      run_main,
      socket,
      text_arguments,
   },
};
use tightrope::{
   client::Client,
   key::Key,
};

use crate::{
   cli::{
      CacheOperation,
      Mode,
   },
   driver::Language,
};

fn dispatch() -> Result<i32> {
   let arguments = args_os().collect::<Vec<_>>();
   let executable = arguments
      .first()
      .and_then(|name| Path::new(name).file_name())
      .unwrap_or_default();
   let mut args = &arguments[1..];
   let name = if executable == "mummer" {
      let Some((command, rest)) = args.split_first() else {
         bail!("mummer requires a mode")
      };
      args = rest;
      command.as_os_str()
   } else {
      executable
   };
   let socket = socket();
   let name_text = name
      .to_str()
      .ok_or_else(|| report!("mode {name:?} is not UTF-8"))?;
   run(Mode::from_arguments(name_text, args)?, args, &socket)
}

fn slot_mode(args: &[OsString], socket: &Path) -> Result<i32> {
   let Some((program, rest)) = args.split_first() else {
      bail!("mummer slot requires a program")
   };
   Ok(process::run_slotted(program, rest, Some(socket))?.status)
}

fn run(mode: Mode, args: &[OsString], socket: &Path) -> Result<i32> {
   match mode {
      Mode::Cc => return Ok(cc::run_cc(Language::Cc, &text_arguments(args)?, socket)?),
      Mode::Cxx => return Ok(cc::run_cc(Language::Cxx, &text_arguments(args)?, socket)?),
      Mode::Fortran => {
         return Ok(cc::run_cc(
            Language::Fortran,
            &text_arguments(args)?,
            socket,
         )?);
      },
      Mode::Rustcwrap => return Ok(rustc::run_rustc(&text_arguments(args)?, socket)?),
      Mode::Ghcwrap => return Ok(ghc::run_ghc(&text_arguments(args)?, socket)?),
      Mode::Tblgenwrap => return Ok(tblgen::run_tblgen(&text_arguments(args)?, socket)?),
      Mode::Cache {
         operation,
         key,
         path,
      } => return cache_mode(operation, &key.parse()?, &path, socket),
      Mode::Slot => return slot_mode(args, socket),
      Mode::Gocacheprog => gocache::run(socket)?,
      Mode::HeaderKey => return header::lookup(&text_arguments(args)?, socket),
      Mode::RelocFixup {
         directory,
         dest,
         sdk,
         deny,
         loader,
      } => {
         fixup::run(
            &directory,
            dest.as_deref(),
            sdk.as_deref(),
            loader.as_deref(),
            &deny,
         )?;
      },
      Mode::NixStore { operation } => nix_store::run(&operation)?,
      Mode::Bootstrap { recipe } => bootstrap::dispatch::mode(&recipe)?,
      Mode::Fetch { kind } => fetch::dispatch::mode(&kind)?,
      Mode::Assemble => fetch::assemble::run_mode()?,
      Mode::WinsdkAssemble => fetch::winsdk_assemble::run_mode()?,
      Mode::Unpack => fetch::unpack::run_mode()?,
   }
   Ok(0)
}

fn cache_mode(operation: CacheOperation, key: &Key, file: &Path, socket: &Path) -> Result<i32> {
   let mut client = match Client::connect(socket) {
      Ok(client) => client,
      Err(error) => {
         diagnostic(format_args!("mummer cache unavailable, {error}"));
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
   run_main(dispatch)
}
