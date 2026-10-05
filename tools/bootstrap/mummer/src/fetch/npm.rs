use std::{
   collections::BTreeMap,
   fs::read_to_string,
   io,
};

use mummer::hash::OutputHash;
use serde::Deserialize;
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
         reject,
      },
      registry::{
         basename,
         url_path,
      },
   },
   nix_store::Store,
};

/// The parts of package-lock.json the producer reads. The lock itself goes
/// back out as a JSON value, so its other fields keep their order.
#[derive(Deserialize)]
struct Lock {
   #[serde(default = "first_version", rename = "lockfileVersion")]
   version:  u64,
   packages: Option<BTreeMap<String, Package>>,
}

const fn first_version() -> u64 {
   1
}

#[derive(Deserialize)]
struct Package {
   resolved:  Option<String>,
   integrity: Option<String>,
   #[serde(default)]
   link:      bool,
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
      let text = read_to_string(lock_path("package-lock.json")?)?;
      let read = serde_json::from_str::<Lock>(&text)?;
      if read.version < 2 {
         return Err(io::Error::other(
            "npmDeps: package-lock.json v1 is not supported (run `npm i --lockfile-version 3`)",
         ));
      }
      let packages = read
         .packages
         .ok_or_else(|| io::Error::other("npmDeps: package-lock.json has no packages"))?;
      // bundled deps have no `resolved`, workspace links resolve to a
      // directory. Everything fetched needs `integrity`
      let fetched_packages = packages
         .iter()
         .filter(|&(_, package)| !package.link)
         .filter_map(|(key, package)| Some((key, package.resolved.as_deref()?, package)))
         .collect::<Vec<_>>();
      let mut foreign = Vec::new();
      let mut unhashed = Vec::new();
      let mut by_url = BTreeMap::new();
      for &(key, resolved, package) in &fetched_packages {
         if !resolved.starts_with("https://") && !resolved.starts_with("http://") {
            foreign.push(key.as_str());
         }
         match package.integrity {
            Some(ref integrity) => {
               by_url.entry(resolved).or_insert(integrity.as_str());
            },
            None => unhashed.push(key.as_str()),
         }
      }
      reject("npmDeps: unsupported `resolved` (git/file) for", &foreign)?;
      reject("npmDeps: no `integrity` for", &unhashed)?;
      // the same tarball can appear under several node_modules paths, fetch
      // once per URL
      let rows = by_url
         .iter()
         .map(|(&url, &integrity)| {
            Ok(Row {
               file: basename(url_path(url)).to_owned(),
               url:  url.to_owned(),
               hash: OutputHash::integrity(integrity)?,
            })
         })
         .collect::<io::Result<Vec<_>>>()?;
      let fetched = fetchurls(store, &rows)?;
      let outs = rows
         .iter()
         .zip(&fetched)
         .map(|(row, made)| (row.url.as_str(), &made.out))
         .collect::<BTreeMap<_, _>>();
      let mut lock = serde_json::from_str::<Map<String, Value>>(&text)?;
      lock.remove("dependencies");
      let entries = lock
         .get_mut("packages")
         .and_then(Value::as_object_mut)
         .ok_or_else(|| io::Error::other("npmDeps: package-lock.json has no packages"))?;
      for (key, resolved, _) in fetched_packages {
         let out = outs
            .get(resolved)
            .ok_or_else(|| io::Error::other(format!("npmDeps: {resolved} was not fetched")))?;
         if let Some(fields) = entries.get_mut(key).and_then(Value::as_object_mut) {
            fields.insert("resolved".to_owned(), Value::String(format!("file:{out}")));
         }
      }
      // structured attrs, the lock is far beyond execve's env limit
      let layout = [json_file("package-lock.json", &lock)?];
      let inputs = fetched.into_iter().map(|made| made.drv).collect::<Vec<_>>();
      collect(store, "npm-deps", &layout, &inputs)
   }
}
