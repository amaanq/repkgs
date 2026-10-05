use std::{
   collections::BTreeMap,
   fs::read_to_string,
   io,
};

use mummer::hash::OutputHash;
use serde_json::{
   Map,
   Value,
};

use crate::{
   fetch::{
      dispatch::Producer,
      drv::{
         Row,
         collect,
         fetchurls,
         json_file,
         lock_path,
         optional,
      },
      registry::{
         basename,
         url_path,
      },
   },
   nix_store::Store,
};

fn text<'lock>(package: &'lock Value, key: &str) -> Option<&'lock str> {
   package.get(key).and_then(Value::as_str)
}

fn linked(package: &Value) -> bool {
   package
      .get("link")
      .and_then(Value::as_bool)
      .unwrap_or(false)
}

/// package-lock.json (v2/v3) -> { package-lock.json } with every `resolved`
/// rewritten to file:<store path>, `npm ci --offline` verifies integrity
/// itself.
pub struct Npm;

impl Producer for Npm {
   fn name(&self) -> &'static str {
      "npm"
   }

   fn run(&self, store: &mut Store) -> io::Result<()> {
      let lock_file = match optional("lockFile") {
         Some(path) => path,
         None => lock_path("package-lock.json")?,
      };
      let mut lock: Map<String, Value> = serde_json::from_str(&read_to_string(&lock_file)?)?;
      if lock
         .get("lockfileVersion")
         .and_then(Value::as_u64)
         .unwrap_or(1)
         < 2
      {
         return Err(io::Error::other(
            "npmDeps: package-lock.json v1 is not supported (run `npm i --lockfile-version 3`)",
         ));
      }
      let packages = lock
         .get("packages")
         .and_then(Value::as_object)
         .ok_or_else(|| io::Error::other("npmDeps: package-lock.json has no packages"))?;
      // bundled deps have no `resolved`, workspace links resolve to a
      // directory. Everything fetched needs `integrity`
      let mut foreign = Vec::new();
      let mut unhashed = Vec::new();
      let mut by_url: BTreeMap<String, String> = BTreeMap::new();
      for (key, package) in packages {
         if linked(package) {
            continue;
         }
         let Some(resolved) = text(package, "resolved") else {
            continue;
         };
         if !resolved.starts_with("https://") && !resolved.starts_with("http://") {
            foreign.push(key.as_str());
         }
         match text(package, "integrity") {
            Some(integrity) => {
               by_url
                  .entry(resolved.to_owned())
                  .or_insert_with(|| integrity.to_owned());
            },
            None => unhashed.push(key.as_str()),
         }
      }
      if !foreign.is_empty() {
         return Err(io::Error::other(format!(
            "npmDeps: unsupported `resolved` (git/file) for: {}",
            foreign.join(", ")
         )));
      }
      if !unhashed.is_empty() {
         return Err(io::Error::other(format!(
            "npmDeps: no `integrity` for: {}",
            unhashed.join(", ")
         )));
      }
      // the same tarball can appear under several node_modules paths, fetch
      // once per URL
      let rows: Vec<Row> = by_url
         .iter()
         .map(|(url, integrity)| {
            Ok(Row {
               file: basename(url_path(url)).to_owned(),
               url:  url.clone(),
               hash: OutputHash::integrity(integrity)?,
            })
         })
         .collect::<io::Result<Vec<_>>>()?;
      let fetched = fetchurls(store, &rows)?;
      let outs: BTreeMap<&str, &str> = rows
         .iter()
         .zip(&fetched)
         .map(|(row, made)| (row.url.as_str(), made.out.as_str()))
         .collect();
      lock.remove("dependencies");
      if let Some(section) = lock.get_mut("packages")
         && let Value::Object(ref mut entries) = *section
      {
         for package in entries.values_mut() {
            if linked(package) {
               continue;
            }
            let Some(resolved) = text(package, "resolved").map(str::to_owned) else {
               continue;
            };
            let out = outs
               .get(resolved.as_str())
               .ok_or_else(|| io::Error::other(format!("npmDeps: {resolved} was not fetched")))?;
            if let Value::Object(ref mut fields) = *package {
               fields.insert("resolved".to_owned(), Value::String(format!("file:{out}")));
            }
         }
      }
      // structured attrs, the lock is far beyond execve's env limit
      let layout = [json_file("package-lock.json", &lock)?];
      let inputs: Vec<String> = fetched.into_iter().map(|made| made.drv).collect();
      collect(store, "npm-deps", &layout, &inputs, None, Map::new(), &[])
   }
}
