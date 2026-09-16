//! `locks.toml [go]` from a source tree's go.sum.
//!
//! Every module version go.sum names, with the sha256 of its proxy.golang.org
//! .zip and .mod (go.sum's h1: is a hash over the zip's file list, not of the
//! zip, so Nix cannot fix a download by it). fetch.goModules turns the lock
//! into one builtin:fetchurl per file; `go` re-verifies h1: against go.sum
//! when it reads them.

use std::{
   collections::BTreeMap,
   fs::read_to_string,
   io,
   path::Path,
};

use jig::tree::parallel;

use crate::{
   locks::Entries,
   nix,
};

/// A module path as the proxy spells it, upper case -> !lower.
pub fn escape(module: &str) -> String {
   let mut escaped = String::with_capacity(module.len());
   for ch in module.chars() {
      if ch.is_ascii_uppercase() {
         escaped.push('!');
         escaped.push(ch.to_ascii_lowercase());
      } else {
         escaped.push(ch);
      }
   }
   escaped
}

struct Sum {
   module:  String,
   version: String,
}

/// go.sum rows keyed `<module>@<version>`, the `/go.mod` suffix dropped.
fn sums(dir: &Path) -> io::Result<BTreeMap<String, Vec<Sum>>> {
   let mut grouped: BTreeMap<String, Vec<Sum>> = BTreeMap::new();
   for line in read_to_string(dir.join("go.sum"))?.lines() {
      let mut fields = line.split(' ');
      let (Some(module), Some(version)) = (fields.next(), fields.next()) else {
         continue;
      };
      if module.is_empty() {
         continue;
      }
      let key = format!("{module}@{}", version.trim_end_matches("/go.mod"));
      grouped.entry(key).or_default().push(Sum {
         module:  module.to_owned(),
         version: version.to_owned(),
      });
   }
   Ok(grouped)
}

struct Locked {
   key:    String,
   fields: BTreeMap<String, String>,
}

/// `{"<module>@<version>": {mod: sri, zip?: sri}}` for the go.sum under
/// `dir`. Hashes already in `old` are not fetched again (another package may
/// have recorded the module mod-only; the zip is added when this go.sum needs
/// it).
pub fn lock(dir: &Path, old: &Entries) -> io::Result<Entries> {
   let groups: Vec<(String, Vec<Sum>)> = sums(dir)?.into_iter().collect();
   let locked = parallel(&groups, |group| {
      let previous = old.get(&group.0).cloned().unwrap_or_default();
      let first = group
         .1
         .first()
         .ok_or_else(|| io::Error::other("empty go.sum group"))?;
      let base = format!(
         "https://proxy.golang.org/{}/@v/{}",
         escape(&first.module),
         escape(first.version.trim_end_matches("/go.mod"))
      );
      // go.sum lists h1 for go.mod alone when only the module graph needed it:
      // no zip then
      let want_zip = group.1.iter().any(|sum| !sum.version.ends_with("/go.mod"));
      let mut fields = BTreeMap::new();
      let module_hash = match previous.get("mod") {
         Some(hash) => hash.clone(),
         None => nix::prefetch_file(&format!("{base}.mod"))?.hash,
      };
      fields.insert("mod".to_owned(), module_hash);
      if let Some(hash) = previous.get("zip") {
         fields.insert("zip".to_owned(), hash.clone());
      } else if want_zip {
         fields.insert(
            "zip".to_owned(),
            nix::prefetch_file(&format!("{base}.zip"))?.hash,
         );
      }
      if fields != previous {
         nix::say(&format!("  {}", group.0));
      }
      Ok(Locked {
         key: group.0.clone(),
         fields,
      })
   })?;
   Ok(locked
      .into_iter()
      .map(|entry| (entry.key, entry.fields))
      .collect())
}
