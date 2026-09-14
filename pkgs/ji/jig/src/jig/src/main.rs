mod cc;
mod directory;
mod driver;
mod elf;
mod exports;
mod files;
mod fixup;
mod gocache;
mod header;
mod image;
mod macho;
mod manifest;
mod nix_store;
mod process;
mod rustc;
mod store;
mod tree;

use std::{
   env::{
      args_os,
      var_os,
   },
   ffi::OsString,
   fs::read,
   io::{
      self,
      Write as _,
   },
   path::{
      Path,
      PathBuf,
   },
   process::ExitCode,
};

use eyre::{
   Result,
   bail,
   eyre,
};
use wire::client::Client;

use crate::{
   directory::{
      get_directory,
      put_directory,
   },
   files::write_file,
   process::diagnostic,
};

fn dispatch() -> Result<i32> {
   let arguments = args_os().collect::<Vec<_>>();
   let executable = arguments
      .first()
      .and_then(|name| Path::new(name).file_name())
      .unwrap_or_default();
   let mut args = &arguments[1..];
   let mode = if executable == "jig" {
      let Some((command, rest)) = args.split_first() else {
         bail!("jig requires a mode")
      };
      args = rest;
      command.as_os_str()
   } else {
      executable
   };
   let socket = var_os("JIG_SOCK").map_or_else(
      || {
         let store = Path::new(option_env!("JIG_STORE_DIR").unwrap_or("/nix/store"));
         store
            .parent()
            .unwrap_or_else(|| Path::new("/nix"))
            .join("var/nix/jigd/socket")
      },
      PathBuf::from,
   );
   if mode == "--help" || mode == "-h" {
      writeln!(
         io::stdout().lock(),
         "jig cc|c++ <args>\njig rustcwrap <rustc> <args>\njig cache get|put <key> <file>\njig \
          cache get-dir|put-dir <key> <directory>\njig slot <program> [args...]\njig \
          gocacheprog\njig header-key <context> <warning-policy> <compiler> <args...>\njig \
          reloc-fixup <directory> [--dest <path>] [--deny <path>]\njig nix-store \
          add-text|add-drv|fetchurls|submit <args>\njig shebangs <directory> [--undo]\njig \
          exports|deps <store path...>\njig write-exports <output>\njig prune <output>\njig \
          relativize <prefix> <store path> [--macho]"
      )?;
      return Ok(0);
   }
   if mode == "cache" {
      return cache_mode(args, &socket);
   }
   if mode == "slot" {
      let Some((program, rest)) = args.split_first() else {
         bail!("jig slot requires a program")
      };
      return Ok(process::run_slotted(program, rest, Some(&socket), false)?.status);
   }
   if mode == "gocacheprog" {
      gocache::run(&socket)?;
      return Ok(0);
   }
   let text_args = args
      .iter()
      .map(|arg| {
         arg.to_str()
            .map(str::to_owned)
            .ok_or_else(|| eyre!("compiler argument is not UTF-8"))
      })
      .collect::<Result<Vec<_>>>()?;
   if mode == "rustcwrap" {
      return Ok(rustc::run_rustc(&text_args, &socket)?);
   }
   if mode == "header-key" {
      return header::lookup(&text_args, &socket);
   }
   if mode == "reloc-fixup" {
      fixup::run(&text_args)?;
      return Ok(0);
   }
   if mode == "nix-store" {
      nix_store::run(&text_args)?;
      return Ok(0);
   }
   if mode == "shebangs" {
      tree::shebangs(&text_args)?;
      return Ok(0);
   }
   if mode == "exports" || mode == "deps" {
      exports::print(&text_args, mode == "deps")?;
      return Ok(0);
   }
   if mode == "write-exports" {
      exports::write(&text_args)?;
      return Ok(0);
   }
   if mode == "prune" {
      tree::prune(&text_args)?;
      return Ok(0);
   }
   if mode == "relativize" {
      tree::relativize(&text_args)?;
      return Ok(0);
   }
   Ok(cc::run_cc(&mode.to_string_lossy(), &text_args, &socket)?)
}

fn cache_mode(args: &[OsString], socket: &Path) -> Result<i32> {
   let [ref operation, ref key, ref file] = *args else {
      bail!("jig cache requires an operation, key, and path");
   };
   let key_text = key
      .to_str()
      .ok_or_else(|| eyre!("cache key is not UTF-8"))?;
   let mut client = match Client::connect(socket) {
      Ok(client) => client,
      Err(error) => {
         diagnostic(format_args!("jig cache unavailable, {error}"));
         return Ok(2);
      },
   };
   match operation.to_str() {
      Some("get") => {
         let Some(bytes) = client.get(key_text)? else {
            return Ok(1);
         };
         write_file(Path::new(file), &bytes, None)?;
      },
      Some("put") => client.put(key_text, &read(file)?)?,
      Some("get-dir") => {
         return Ok(i32::from(!get_directory(
            &mut client,
            key_text,
            Path::new(file),
         )?));
      },
      Some("put-dir") => put_directory(&mut client, key_text, Path::new(file))?,
      _ => {
         bail!(
            "unknown jig cache operation {}",
            operation.to_string_lossy()
         )
      },
   }
   Ok(0)
}

fn main() -> ExitCode {
   if let Err(error) = color_eyre::install() {
      diagnostic(format_args!(
         "jig could not install error reporting, {error}"
      ));
      return ExitCode::FAILURE;
   }
   match dispatch() {
      Ok(status) => ExitCode::from(u8::try_from(status).unwrap_or(1)),
      Err(error) => {
         diagnostic(format_args!("{error:#}"));
         ExitCode::FAILURE
      },
   }
}
