use std::{
   collections::BTreeMap,
   env::var,
   fs::{
      canonicalize,
      create_dir_all,
      read,
      read_to_string,
      remove_dir_all,
   },
   io,
   path::Path,
   process::Command,
};

use mummer::{
   directory::{
      get_directory,
      put_directory,
   },
   files::write_file,
   tree::{
      note,
      walk,
   },
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

/// Leaves `$out` out, since a CA derivation's is a placeholder that changes
/// with every input, and `store` keeps whatever names it out of the cache.
pub fn key<Extra>(kind: &str, scripts: &[String], extra: Option<&Extra>) -> io::Result<String>
where
   Extra: Serialize,
{
   let top = build_top()?;
   let ctx = Ctx::load(&top)?;
   let mut sorted = scripts.to_vec();
   sorted.sort();
   let files = sorted
      .iter()
      .map(|path| Ok(format!("{path}={}", hex(&read(path)?))))
      .collect::<io::Result<Vec<_>>>()?;
   let cc = match var("PKGS_CC") {
      Ok(flags) => serde_json::from_str::<BTreeMap<String, Value>>(&flags)?,
      Err(_) => BTreeMap::new(),
   };
   let toolchains = cc
      .keys()
      .map(|root| toolchain(root, &top))
      .collect::<io::Result<Vec<_>>>()?;
   let roots = ctx
      .roots
      .iter()
      .filter(|root| !cc.contains_key(root.as_str()))
      .collect::<Vec<_>>();
   let record = json!({
      "files": files,
      "args": extra,
      "roots": roots,
      "cc": cc.values().collect::<Vec<_>>(),
      "toolchains": toolchains,
   });
   Ok(format!(
      "probe/{kind}/{}",
      hex(&serde_json::to_vec(&record)?)
   ))
}

/// The cc toolchain's files and a program it links, its own path masked and
/// its mummer binary left out, so a driver change re-runs probes and a
/// cache-only one does not.
fn toolchain(root: &str, top: &str) -> io::Result<String> {
   let wrapper = canonicalize(Path::new(root).join("bin/cc"))?;
   let sample = Path::new(top).join(".probe-sample");
   create_dir_all(&sample)?;
   write_file(
      &sample.join("main.c"),
      b"int main(void) { return 0; }\n",
      None,
   )?;
   let linked = Command::new(Path::new(root).join("bin/cc"))
      .args(["main.c", "-o", "main"])
      .current_dir(&sample)
      .output()?;
   if !linked.status.success() {
      return Err(io::Error::other(format!(
         "cc could not link the probe sample: {}",
         String::from_utf8_lossy(&linked.stderr).trim()
      )));
   }
   let mut hasher = blake3::Hasher::new();
   let mut masked = |bytes: &[u8]| {
      let mut rest = bytes;
      while let Some(position) = rest
         .windows(root.len())
         .position(|window| window == root.as_bytes())
      {
         hasher.update(&rest[..position]);
         hasher.update(b"/cc");
         rest = &rest[position + root.len()..];
      }
      hasher.update(rest);
   };
   masked(&read(sample.join("main"))?);
   remove_dir_all(&sample)?;
   for entry in walk(Path::new(root))? {
      if Path::new(&entry.path) == wrapper {
         continue;
      }
      masked(format!("{}:{}:{}\0", entry.kind, entry.rel, entry.target).as_bytes());
      if entry.kind == 'f' {
         masked(&read(&entry.path)?);
      }
   }
   Ok(hasher.finalize().to_hex().to_string())
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
   let text = read_to_string(file)?;
   let kept = text
      .split_inclusive('\n')
      .filter(|line| !line.contains(&ctx.out))
      .collect::<String>();
   if let Err(error) = client.put(key, kept.as_bytes()) {
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
   for entry in walk(dir)? {
      if entry.kind == 'f'
         && read(&entry.path)?
            .windows(ctx.out.len())
            .any(|window| window == ctx.out.as_bytes())
      {
         note(
            "probe-cache",
            &format!("not stored, {} names $out", entry.rel),
         );
         return Ok(());
      }
   }
   put_directory(&mut client, key, dir)
}

pub fn cmake_init(cache: &Path) -> io::Result<String> {
   let bracket = |text: &str| {
      let mut delimiter = String::new();

      // a value ending in `]` would close `[[x]]]` one bracket early
      while format!("{text}]").contains(&format!("]{delimiter}]")) {
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
