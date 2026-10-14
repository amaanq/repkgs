use std::{
   collections::BTreeMap,
   env::vars,
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
   config::{
      PackageFlags,
      package_flags,
   },
   directory::{
      get_directory,
      put_directory,
   },
   files::{
      utf8,
      write_file,
   },
   process::{
      loose,
      strict,
   },
   store::{
      STORE_DIR,
      mask_hashes,
   },
   tree::{
      EntryKind,
      find,
      note,
      walk,
   },
};
use serde::Serialize;
use tightrope::{
   client::Client,
   key::Key,
};

use crate::{
   attrs::{
      Attrs,
      Ctx,
      build_top,
   },
   build::nu_internal,
};

#[derive(Serialize)]
struct Record<'inputs, Extra> {
   files:       Vec<String>,
   environment: BTreeMap<String, String>,
   args:        Option<&'inputs Extra>,
   roots:       Vec<&'inputs String>,
   cc:          Vec<&'inputs PackageFlags>,
   toolchains:  Vec<String>,
}

/// Leaves `$out` out, since a CA derivation's is a placeholder that changes
/// with every input, and `store` keeps whatever names it out of the cache.
pub fn key<Script, Extra>(kind: &str, scripts: &[Script], extra: Option<&Extra>) -> io::Result<Key>
where
   Script: AsRef<str>,
   Extra: Serialize,
{
   let top = build_top()?;
   let ctx = Ctx::load(&top)?;
   let mut sorted = scripts.iter().map(AsRef::as_ref).collect::<Vec<&str>>();
   sorted.sort_unstable();
   let files = sorted
      .iter()
      .map(|path| Ok(format!("{path}={}", hex(&read(path)?))))
      .collect::<io::Result<Vec<_>>>()?;
   let cc = package_flags()?;
   let toolchains = cc
      .keys()
      .map(|root| toolchain(root, &top))
      .collect::<io::Result<Vec<_>>>()?;
   let plumbing = Attrs::load()?.plumbing;
   let roots = ctx
      .roots
      .iter()
      .filter(|root| !cc.contains_key(root.as_str()) && !plumbing.contains(root))
      .collect::<Vec<_>>();
   // what configure inherits from bigtop, so a change to it reruns probes
   // while bigtop's own store path stays out
   let environment = vars()
      .filter_map(|(name, value)| {
         inherited(&name).then(|| {
            let masked = mask_hashes(&value);
            (name, masked)
         })
      })
      .collect();
   let record = Record {
      files,
      environment,
      args: extra,
      roots,
      cc: cc.values().collect(),
      toolchains,
   };
   format!("probe/{kind}/{}", hex(&serde_json::to_vec(&record)?)).parse()
}

pub fn inherited(name: &str) -> bool {
   !nu_internal(name)
      && !matches!(
         name,
         "NIX_LOG_FD" | "NIX_BUILD_CORES" | "TERM" | "SHLVL" | "_"
      )
}

/// The cc toolchain's files and a program it links, its own path masked and
/// its mummer binary left out, so a driver change re-runs probes and a
/// cache-only one does not.
pub fn toolchain(root: &str, top: &str) -> io::Result<String> {
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
         loose(&linked.stderr).trim()
      )));
   }
   let mut hasher = blake3::Hasher::new();
   let mut masked = |bytes: &[u8]| {
      let mut rest = bytes;
      while let Some(position) = find(rest, root.as_bytes()) {
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
      if entry.kind == EntryKind::File {
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

pub fn restore(key: &Key, file: &Path, socket: &Path) -> io::Result<bool> {
   let ctx = Ctx::load(&build_top()?)?;
   let Some(mut client) = daemon(&ctx, socket) else {
      return Ok(false);
   };
   let Some(bytes) = client.get(key)? else {
      return Ok(false);
   };
   let Some(text) = resolved(&ctx.roots, strict(&bytes, "a stored probe result")?) else {
      note(
         "probe-cache",
         "stored results name a store path this build lacks",
      );
      return Ok(false);
   };
   write_file(file, text.as_bytes(), None)?;
   Ok(true)
}

/// Stored results with their store hashes put back from this build's roots,
/// or None when they name any store path outside those roots, as results
/// stored by another toolchain do.
pub fn resolved(roots: &[String], text: &str) -> Option<String> {
   let mut resolved = text.to_owned();
   for root in roots {
      let masked = mask_hashes(root);
      let mut result = String::with_capacity(resolved.len());
      let mut remaining = resolved.as_str();
      while let Some(position) = remaining.find(&masked) {
         let end = position + masked.len();
         let whole = remaining[end..]
            .chars()
            .next()
            .is_none_or(|next| !next.is_ascii_alphanumeric() && !"-._+?=".contains(next));
         result.push_str(&remaining[..position]);
         result.push_str(if whole { root } else { &masked });
         remaining = &remaining[end..];
      }
      result.push_str(remaining);
      resolved = result;
   }
   let prefix = format!("{STORE_DIR}/");
   let known = resolved.match_indices(&prefix).all(|(position, _)| {
      let tail = &resolved[position + prefix.len()..];
      let name = tail.split('/').next().unwrap_or(tail);
      roots.iter().any(|root| {
         root.strip_prefix(&prefix) == Some(name) || tail.starts_with(&root[prefix.len()..])
      })
   });
   known.then_some(resolved)
}

pub fn store(key: &Key, file: &Path, socket: &Path) -> io::Result<()> {
   let ctx = Ctx::load(&build_top()?)?;
   if !file.exists() {
      return Ok(());
   }
   let Some(mut client) = daemon(&ctx, socket) else {
      return Ok(());
   };
   let text = read_to_string(file)?;
   let out = utf8(&ctx.dest)?;
   let kept = mask_hashes(
      &text
         .split_inclusive('\n')
         .filter(|line| !line.contains(out))
         .collect::<String>(),
   );
   if let Err(error) = client.put(key, kept.as_bytes()) {
      note("probe-cache", &format!("store failed with {error}"));
   }
   Ok(())
}

pub fn restore_dir(key: &Key, dir: &Path, socket: &Path) -> io::Result<bool> {
   let ctx = Ctx::load(&build_top()?)?;
   let Some(mut client) = daemon(&ctx, socket) else {
      return Ok(false);
   };
   // a cache the daemon cannot give back is a cold one, not a failed build
   get_directory(&mut client, key, dir).or_else(|error| {
      note("probe-cache", &format!("restore failed with {error}"));
      Ok(false)
   })
}

pub fn store_dir(key: &Key, dir: &Path, socket: &Path) -> io::Result<()> {
   let ctx = Ctx::load(&build_top()?)?;
   if !dir.exists() {
      return Ok(());
   }
   let Some(mut client) = daemon(&ctx, socket) else {
      return Ok(());
   };
   // the install prefix is the same in every build, only the store output
   // differs
   let out = ctx.dest.as_os_str().as_encoded_bytes();
   for entry in walk(dir)? {
      if entry.kind == EntryKind::File && find(&read(&entry.path)?, out).is_some() {
         note(
            "probe-cache",
            &format!("not stored, {} names $out", entry.rel),
         );
         return Ok(());
      }
   }
   if let Err(error) = put_directory(&mut client, key, dir) {
      note("probe-cache", &format!("store failed with {error}"));
   }
   Ok(())
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
