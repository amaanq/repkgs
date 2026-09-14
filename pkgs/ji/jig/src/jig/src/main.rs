mod cc;
mod cli;
mod debug;
mod directory;
mod driver;
mod elf;
mod env;
mod exports;
mod files;
mod fixup;
mod gocache;
mod header;
mod image;
mod implant;
mod launchers;
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
use pound::Error as ArgumentError;
use wire::client::Client;

use crate::{
   cli::{
      CacheOperation,
      Mode,
      text_arguments,
   },
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
   let name = if executable == "jig" {
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
   let name_text = name
      .to_str()
      .ok_or_else(|| eyre!("mode {name:?} is not UTF-8"))?;
   let mode = Mode::from_arguments(name_text, args)?;
   match mode {
      Mode::Cc | Mode::Cxx => return Ok(cc::run_cc(name_text, &text_arguments(args)?, &socket)?),
      Mode::Rustcwrap => return Ok(rustc::run_rustc(&text_arguments(args)?, &socket)?),
      Mode::Cache {
         operation,
         key,
         path,
      } => return cache_mode(operation, &key, &path, &socket),
      Mode::Slot => {
         let Some((program, rest)) = args.split_first() else {
            bail!("jig slot requires a program")
         };
         return Ok(process::run_slotted(program, rest, Some(&socket), false)?.status);
      },
      Mode::Gocacheprog => gocache::run(&socket)?,
      Mode::HeaderKey => return header::lookup(&text_arguments(args)?, &socket),
      Mode::RelocFixup {
         directory,
         dest,
         deny,
      } => fixup::run(&directory, dest.as_deref(), &deny)?,
      Mode::NixStore { operation } => nix_store::run(&operation)?,
      Mode::Shebangs { directory, undo } => tree::shebangs(&directory, undo)?,
      Mode::Exports { roots } => exports::print(&roots, false)?,
      Mode::Deps { roots } => exports::print(&roots, true)?,
      Mode::WriteExports { output } => exports::write(&output)?,
      Mode::Prune { output } => tree::prune(&output)?,
      Mode::Relativize {
         prefix,
         dest,
         macho,
      } => tree::relativize(&prefix, &dest, macho)?,
      Mode::SplitDebug { output, debug } => debug::run(&output, &debug)?,
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
   if let Err(error) = color_eyre::install() {
      diagnostic(format_args!(
         "jig could not install error reporting, {error}"
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
