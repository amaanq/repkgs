use std::{
   env::{
      var,
      vars,
   },
   fs::{
      read,
      read_to_string,
   },
   io,
   path::Path,
};

use serde_json::{
   Value,
   json,
};
use wire::client::Client;

use crate::{
   attrs::{
      Attrs,
      Ctx,
      build_top,
   },
   directory::{
      get_directory,
      put_directory,
   },
   files::write_file,
   identify::{
      identities,
      masked_hashes,
   },
   tree::replace_all,
};

const PRIVATE: [&str; 4] = ["JIG_LOG", "JIG_LOG_ARGS", "JIG_SOCK", "JIG_SLOT"];

pub fn key(
   kind: &str,
   scripts: &[String],
   extra: Option<&Value>,
   no_out: bool,
   socket: &Path,
) -> io::Result<String> {
   let attrs = Attrs::load()?;
   let ctx = Ctx::load(&build_top()?)?;
   let mut environment: Vec<(String, String)> = vars()
      .filter(|pair| !PRIVATE.contains(&pair.0.as_str()))
      .collect();
   environment.sort();
   let sites: Vec<Value> = var("CONFIG_SITE")
      .unwrap_or_default()
      .split_whitespace()
      .map(|site| {
         json!({
            "path": site,
            "hash": read(site).ok().map(|bytes| hex(&bytes)),
         })
      })
      .collect();
   let mut by_content: Vec<String> = Vec::new();
   for path in ctx
      .roots
      .iter()
      .chain(&attrs.probe_inputs)
      .chain([&attrs.src])
      .chain(&attrs.patches)
   {
      if !path.is_empty() && !by_content.contains(path) {
         by_content.push(path.clone());
      }
   }
   let identity = identities(&by_content, socket)?;
   let of = |path: &String| identity.get(path).cloned().unwrap_or(Value::Null);
   let mut sorted = scripts.to_vec();
   sorted.sort();
   let script: Vec<Value> = sorted
      .iter()
      .map(|path| Ok(json!({ "path": path, "hash": hex(&read(path)?) })))
      .collect::<io::Result<_>>()?;
   let record = json!({
      "kind": kind,
      "script": script,
      "source": of(&attrs.src),
      "patches": attrs.patches.iter().map(of).collect::<Vec<_>>(),
      "recipes": attrs.probe_inputs.iter().map(of).collect::<Vec<_>>(),
      "spec": ctx.spec,
      "platform": ctx.platform,
      "roots": ctx.roots.iter().map(of).collect::<Vec<_>>(),
      "out": if no_out { Value::Null } else { Value::String(ctx.out.clone()) },
      "build": ctx.build,
      "environment": environment.iter().map(|pair| json!({ "name": pair.0, "value": pair.1 })).collect::<Vec<_>>(),
      "sites": sites,
      "extra": extra.cloned().unwrap_or(Value::Null),
   });
   let text = masked_hashes(serde_json::to_vec(&record)?);
   Ok(format!("probe/v3/{kind}/{}", hex(&text)))
}

fn hex(bytes: &[u8]) -> String {
   blake3::hash(bytes).to_hex().to_string()
}

fn masked(ctx: &Ctx, text: &[u8], undo: bool) -> Vec<u8> {
   let mut result = text.to_vec();
   for (index, root) in ctx.roots.iter().enumerate() {
      let placeholder = format!("{{root{index}}}");
      result = if undo {
         replace_all(&result, placeholder.as_bytes(), root.as_bytes())
      } else {
         replace_all(&result, root.as_bytes(), placeholder.as_bytes())
      };
   }
   result
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
   write_file(file, &masked(&ctx, &bytes, true), None)?;
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
   client.put(key, &masked(&ctx, &read(file)?, false))
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
   let mut lines = Vec::new();
   for line in read_to_string(cache)?.lines() {
      let Some((name, value)) = line.split_once(":INTERNAL=") else {
         continue;
      };
      if name.is_empty()
         || !name
            .chars()
            .all(|found| found.is_ascii_alphanumeric() || found == '_')
         || name.starts_with("CMAKE_")
         || name.ends_with("-ADVANCED")
         || value.contains('/')
      {
         continue;
      }
      let mut delimiter = String::from("=");
      while value.contains(&format!("]{delimiter}]")) {
         delimiter.push('=');
      }
      lines.push(format!(
         "set({name} [{delimiter}[{value}]{delimiter}] CACHE INTERNAL \"\")"
      ));
   }
   Ok(lines.join("\n"))
}
