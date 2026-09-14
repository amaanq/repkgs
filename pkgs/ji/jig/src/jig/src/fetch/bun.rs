use std::{
   collections::BTreeMap,
   fs::read_to_string,
   io,
};

use serde::{
   Deserialize,
   Serialize,
};
use serde_json::{
   Map,
   Value,
};

use crate::{
   fetch::{
      dispatch::Producer,
      drv::{
         Entry,
         Hash,
         Row,
         collect,
         fetchurls,
         json_file,
         lock_path,
      },
      registry::{
         flat_name,
         split_id,
         tarball_url,
      },
   },
   nix_store::Store,
};

#[derive(Deserialize)]
struct Lock {
   packages: BTreeMap<String, Vec<Value>>,
}

#[derive(Serialize)]
struct Indexed<'entry> {
   id:  &'entry str,
   dir: String,
}

/// bun writes trailing commas but no comments.
fn strip_trailing_commas(text: &str) -> String {
   let mut cleaned = String::with_capacity(text.len());
   let mut in_string = false;
   let mut escaped = false;
   let mut pending_comma = false;
   let mut held = String::new();
   for character in text.chars() {
      if in_string {
         cleaned.push(character);
         if escaped {
            escaped = false;
         } else if character == '\\' {
            escaped = true;
         } else if character == '"' {
            in_string = false;
         }
         continue;
      }
      if pending_comma {
         if character.is_whitespace() {
            held.push(character);
            continue;
         }
         if character != '}' && character != ']' {
            cleaned.push(',');
         }
         cleaned.push_str(&held);
         held.clear();
         pending_comma = false;
      }
      match character {
         ',' => pending_comma = true,
         '"' => {
            in_string = true;
            cleaned.push(character);
         },
         _ => cleaned.push(character),
      }
   }
   if pending_comma {
      cleaned.push(',');
      cleaned.push_str(&held);
   }
   cleaned
}

/// bun.lock -> { p/<name>@<version>/ (unpacked), index.json }, linked into
/// `$BUN_INSTALL_CACHE_DIR` under the names bun expects.
pub struct Bun;

impl Producer for Bun {
   fn name(&self) -> &'static str {
      "bun"
   }

   fn run(&self, store: &mut Store) -> io::Result<()> {
      let text = read_to_string(lock_path("bun.lock")?)?;
      let lock: Lock = serde_json::from_str(&strip_trailing_commas(&text))?;
      let mut unhashed = Vec::new();
      let mut rows = Vec::new();
      let mut ids: Vec<String> = Vec::new();
      let mut dirs: Vec<String> = Vec::new();
      for entry in lock.packages.values() {
         let id = entry.first().and_then(Value::as_str).unwrap_or_default();
         let registry = entry.get(1).and_then(Value::as_str).unwrap_or_default();
         let hashed = entry
            .last()
            .and_then(Value::as_str)
            .filter(|last| last.starts_with("sha512-"));
         let Some(integrity) = hashed else {
            if !id.contains("@workspace:") {
               unhashed.push(id);
            }
            continue;
         };
         if ids.iter().any(|seen| seen == id) {
            continue;
         }
         let (name, version) = split_id(id);
         ids.push(id.to_owned());
         dirs.push(format!("p/{}@{version}", flat_name(name)));
         rows.push(Row {
            file: format!("{}-{version}.tgz", flat_name(name)),
            url:  tarball_url(name, version, registry),
            hash: Hash::Sri(integrity.to_owned()),
         });
      }
      if !unhashed.is_empty() {
         return Err(io::Error::other(format!(
            "bunDeps: bun.lock has no integrity for {} (git/github/file dependencies)",
            unhashed.join(", ")
         )));
      }
      let fetched = fetchurls(store, &rows)?;
      let mut layout: Vec<Entry> = dirs
         .iter()
         .zip(&fetched)
         .map(|(dir, made)| Entry::unpack(made.out.clone(), dir.clone()))
         .collect();
      let index: Vec<Indexed<'_>> = ids
         .iter()
         .zip(&dirs)
         .map(|(id, dir)| {
            Indexed {
               id,
               dir: dir.clone(),
            }
         })
         .collect();
      layout.push(json_file("index.json", &index)?);
      let inputs: Vec<String> = fetched.into_iter().map(|made| made.drv).collect();
      collect(store, "bun-deps", &layout, &inputs, None, Map::new())
   }
}
