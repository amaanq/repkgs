use std::{
   collections::BTreeSet,
   fs::read_to_string,
   io,
};

use mummer::{
   hash::OutputHash,
   store_path::DrvPath,
};
use serde_json::Map;

use crate::{
   fetch::{
      dispatch::Producer,
      drv::{
         Entry,
         Row,
         collect,
         fetchurls,
         lock_path,
      },
      registry::{
         basename,
         url_path,
      },
   },
   nix_store::Store,
};

struct Locked {
   keys:      String,
   resolved:  Option<String>,
   integrity: Option<String>,
}

/// yarn.lock v1, unindented `"a@^1", b@2:` headers, two-space indented
/// `field value` lines, values optionally quoted.
fn parse_lock(text: &str) -> io::Result<Vec<Locked>> {
   if text.contains("__metadata:") {
      return Err(io::Error::other(
         "yarnDeps: yarn.lock is yarn berry (v2+), only yarn classic v1 is supported",
      ));
   }
   let mut entries: Vec<Locked> = Vec::new();
   for line in text.lines() {
      if line.is_empty() || line.starts_with('#') {
         continue;
      }
      if !line.starts_with(' ') {
         entries.push(Locked {
            keys:      line.trim_end_matches(':').to_owned(),
            resolved:  None,
            integrity: None,
         });
         continue;
      }
      let Some((key, value)) = line.trim().split_once(' ') else {
         continue;
      };
      let Some(entry) = entries.last_mut() else {
         continue;
      };
      let field = value.trim_matches('"').to_owned();
      match key {
         "resolved" => entry.resolved = Some(field),
         "integrity" => entry.integrity = Some(field),
         _ => {},
      }
   }
   Ok(entries)
}

/// the offline-mirror file name yarn derives from a tarball URL.
fn mirror_name(url: &str) -> String {
   let path = url_path(url);
   let base = basename(path);
   let scope = path
      .strip_prefix("/@")
      .and_then(|rest| rest.split('/').next())
      .filter(|found| !found.is_empty());
   scope.map_or_else(|| base.to_owned(), |found| format!("@{found}-{base}"))
}

fn foreign(resolved: &str) -> bool {
   !(resolved.starts_with("https://") || resolved.starts_with("http://"))
      || resolved.contains(".git#")
      || resolved.strip_suffix(".git").is_some()
      || resolved.contains("/tarball/")
      || resolved.contains("codeload.github.com")
}

/// yarn.lock (classic v1) -> a yarn offline mirror directory.
pub struct Yarn;

impl Producer for Yarn {
   fn name(&self) -> &'static str {
      "yarn"
   }

   fn run(&self, store: &mut Store) -> io::Result<()> {
      let entries = parse_lock(&read_to_string(lock_path("yarn.lock")?)?)?;
      // workspace / link: / file: entries have no `resolved`
      let remote: Vec<&Locked> = entries
         .iter()
         .filter(|entry| entry.resolved.is_some())
         .collect();
      let bad: Vec<&str> = remote
         .iter()
         .filter(|entry| entry.resolved.as_deref().is_some_and(foreign))
         .map(|entry| entry.keys.as_str())
         .collect();
      if !bad.is_empty() {
         return Err(io::Error::other(format!(
            "yarnDeps: unsupported `resolved` (git/github/file) for: {}",
            bad.join(", ")
         )));
      }
      let unhashed: Vec<&str> = remote
         .iter()
         .filter(|entry| entry.integrity.is_none())
         .map(|entry| entry.keys.as_str())
         .collect();
      if !unhashed.is_empty() {
         return Err(io::Error::other(format!(
            "yarnDeps: no `integrity` (yarn < 1.10 lock, run `yarn install` once with a current \
             yarn 1.x) for: {}",
            unhashed.join(", ")
         )));
      }
      let mut seen = BTreeSet::new();
      let mut rows = Vec::new();
      for entry in remote {
         let url = entry
            .resolved
            .as_deref()
            .unwrap_or_default()
            .split('#')
            .next()
            .unwrap_or_default()
            .to_owned();
         let file = mirror_name(&url);
         if !seen.insert(file.clone()) {
            continue;
         }
         rows.push(Row {
            file,
            url,
            hash: OutputHash::integrity(entry.integrity.as_deref().unwrap_or_default())?,
         });
      }
      let fetched = fetchurls(store, &rows)?;
      let layout: Vec<Entry> = rows
         .iter()
         .zip(&fetched)
         .map(|(row, made)| Entry::link(made.out.to_string(), row.file.clone()))
         .collect();
      let inputs: Vec<DrvPath> = fetched.into_iter().map(|made| made.drv).collect();
      collect(store, "yarn-deps", &layout, &inputs, None, Map::new(), &[])
   }
}
