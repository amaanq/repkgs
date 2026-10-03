use std::{
   env::var,
   fs::{
      read,
      read_to_string,
   },
   io,
   path::Path,
};

use mummer::{
   directory::{
      get_directory,
      put_directory,
   },
   files::write_file,
   tree::note,
};
use serde::Serialize;
use serde_json::{
   Value,
   json,
};
use tightrope::client::Client;

use crate::attrs::{
   Ctx,
   build_top,
};

/// `$out` unless `no_out`, since configure records the prefix.
pub fn key<Extra>(
   kind: &str,
   scripts: &[String],
   extra: Option<&Extra>,
   no_out: bool,
) -> io::Result<String>
where
   Extra: Serialize,
{
   let ctx = Ctx::load(&build_top()?)?;
   let mut sorted = scripts.to_vec();
   sorted.sort();
   let files = sorted
      .iter()
      .map(|path| Ok(hex(&read(path)?)))
      .collect::<io::Result<Vec<_>>>()?;
   let cc: Value = match var("PKGS_CC") {
      Ok(flags) => serde_json::from_str(&flags)?,
      Err(_) => Value::Null,
   };
   let record = json!({
      "files": files,
      "args": extra,
      "roots": ctx.roots,
      "cc": cc,
      "out": if no_out { None } else { Some(&ctx.out) },
   });
   Ok(format!(
      "probe/v4/{kind}/{}",
      hex(&serde_json::to_vec(&record)?)
   ))
}

fn hex(bytes: &[u8]) -> String {
   blake3::hash(bytes).to_hex().to_string()
}

fn daemon(ctx: &Ctx, socket: &Path) -> Option<Client> {
   if !ctx.cache {
      return None;
   }
   Client::connect(socket).ok()
}

pub fn restore(key: &str, file: &Path, socket: &Path) -> io::Result<bool> {
   let ctx = Ctx::load(&build_top()?)?;
   let Some(mut client) = daemon(&ctx, socket) else {
      return Ok(false);
   };
   let Some(bytes) = client.get(key)? else {
      return Ok(false);
   };
   write_file(file, &bytes, None)?;
   Ok(true)
}

pub fn store(key: &str, file: &Path, socket: &Path) -> io::Result<()> {
   let ctx = Ctx::load(&build_top()?)?;
   if !file.exists() {
      return Ok(());
   }
   let Some(mut client) = daemon(&ctx, socket) else {
      return Ok(());
   };
   if let Err(error) = client.put(key, &read(file)?) {
      note("probe-cache", &format!("store failed with {error}"));
   }
   Ok(())
}

pub fn restore_dir(key: &str, dir: &Path, socket: &Path) -> io::Result<bool> {
   let ctx = Ctx::load(&build_top()?)?;
   let Some(mut client) = daemon(&ctx, socket) else {
      return Ok(false);
   };
   get_directory(&mut client, key, dir)
}

pub fn store_dir(key: &str, dir: &Path, socket: &Path) -> io::Result<()> {
   let ctx = Ctx::load(&build_top()?)?;
   if !dir.exists() {
      return Ok(());
   }
   let Some(mut client) = daemon(&ctx, socket) else {
      return Ok(());
   };
   put_directory(&mut client, key, dir)
}

pub fn cmake_init(cache: &Path) -> io::Result<String> {
   let bracket = |text: &str| {
      let mut delimiter = String::new();

      while text.contains(&format!("]{delimiter}]")) {
         delimiter.push('=');
      }

      format!("[{delimiter}[{text}]{delimiter}]")
   };
   let mut lines = Vec::new();

   for line in read_to_string(cache)?.lines() {
      if line.starts_with('#') || line.starts_with("//") {
         continue;
      }

      let entry = line.strip_prefix('"').map_or_else(
         || line.split_once(':').filter(|entry| !entry.0.contains('=')),
         |quoted| quoted.split_once("\":"),
      );
      let Some((name, body)) = entry else {
         continue;
      };

      let Some(value) = body.strip_prefix("INTERNAL=") else {
         continue;
      };

      if name.is_empty()
         || name.starts_with("CMAKE_")
         || ["-ADVANCED", "-STRINGS", "-MODIFIED"]
            .iter()
            .any(|suffix| name.ends_with(suffix))
      {
         continue;
      }

      let trimmed = value.trim_end_matches([' ', '\t', '\r']);
      let unquoted = trimmed
         .strip_prefix('\'')
         .and_then(|rest| rest.strip_suffix('\''))
         .unwrap_or(trimmed);
      lines.push(format!(
         "set({} {} CACHE INTERNAL \"\")",
         bracket(name),
         bracket(unquoted),
      ));
   }

   Ok(lines.join("\n"))
}
