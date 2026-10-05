use std::{
   collections::{
      BTreeMap,
      BTreeSet,
   },
   fs::read_to_string,
   io,
};

use mummer::{
   hash::OutputHash,
   store_path::DrvPath,
};
use serde::{
   Deserialize,
   Serialize,
};
use serde_json::Map;

use crate::{
   fetch::{
      dispatch::Producer,
      drv::{
         Entry,
         Row,
         attr,
         collect,
         fetchurls,
         json_file,
         lock_path,
      },
   },
   nix_store::Store,
};

const PROXY: &str = "https://proxy.golang.org";

#[derive(Deserialize)]
struct Locks {
   go: BTreeMap<String, BTreeMap<String, String>>,
}

#[derive(Serialize)]
struct Info<'module> {
   #[serde(rename = "Version")]
   version: &'module str,
}

struct Module {
   path:    String,
   version: String,
}

/// one entry per module version, go.sum lists most twice (tree and /go.mod).
fn go_sum(text: &str) -> Vec<Module> {
   let mut seen = BTreeSet::new();
   let mut modules = Vec::new();
   for line in text.lines().filter(|line| !line.is_empty()) {
      let mut words = line.split(' ');
      let path = words.next().unwrap_or_default();
      let version = words.next().unwrap_or_default().trim_end_matches("/go.mod");
      if seen.insert((path.to_owned(), version.to_owned())) {
         modules.push(Module {
            path:    path.to_owned(),
            version: version.to_owned(),
         });
      }
   }
   modules
}

/// the proxy's case encoding for module paths and versions (v1.0.0-RC1).
fn proxy_case(path: &str) -> String {
   let mut encoded = String::with_capacity(path.len());
   for character in path.chars() {
      if character.is_ascii_uppercase() {
         encoded.push('!');
         encoded.push(character.to_ascii_lowercase());
      } else {
         encoded.push(character);
      }
   }
   encoded
}

/// go.sum -> a GOPROXY=file:// tree, <module>/@v/<version>.{info,mod,zip},
/// the .mod/.zip sha256s from locks/go.toml.
pub struct Go;

impl Producer for Go {
   fn name(&self) -> &'static str {
      "go"
   }

   fn run(&self, store: &mut Store) -> io::Result<()> {
      let locks: Locks =
         toml::from_str(&read_to_string(attr("locks")?)?).map_err(io::Error::other)?;
      let modules = go_sum(&read_to_string(lock_path("go.sum")?)?);
      let missing: Vec<String> = modules
         .iter()
         .map(|module| format!("{}@{}", module.path, module.version))
         .filter(|key| !locks.go.contains_key(key))
         .collect();
      if !missing.is_empty() {
         return Err(io::Error::other(format!(
            "goModules: {} modules not in locks/go.toml (`spieler lock <pkg>` adds them): {}",
            missing.len(),
            missing
               .iter()
               .take(5)
               .cloned()
               .collect::<Vec<_>>()
               .join(" ")
         )));
      }
      let mut rows = Vec::new();
      let mut targets = Vec::new();
      for module in &modules {
         let (path, version) = (&module.path, &module.version);
         let dir = format!("{}/@v", proxy_case(path));
         let hashes = locks
            .go
            .get(&format!("{path}@{version}"))
            .ok_or_else(|| io::Error::other(format!("goModules: {path}@{version} vanished")))?;
         for (ext, sri) in hashes {
            let file = format!("{}.{ext}", proxy_case(version));
            targets.push(format!("{dir}/{file}"));
            rows.push(Row {
               file: format!("{path}-{file}"),
               url:  format!("{PROXY}/{dir}/{file}"),
               hash: OutputHash::integrity(sri)?,
            });
         }
      }
      let fetched = fetchurls(store, &rows)?;
      let mut layout: Vec<Entry> = targets
         .iter()
         .zip(&fetched)
         .map(|(to, made)| Entry::link(made.out.to_string(), to.clone()))
         .collect();
      for module in &modules {
         let (path, version) = (&module.path, &module.version);
         layout.push(json_file(
            &format!("{}/@v/{}.info", proxy_case(path), proxy_case(version)),
            &Info { version },
         )?);
      }
      let inputs: Vec<DrvPath> = fetched.into_iter().map(|made| made.drv).collect();
      collect(store, "go-modules", &layout, &inputs, None, Map::new(), &[])
   }
}
