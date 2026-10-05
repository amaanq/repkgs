use std::{
   collections::{
      BTreeMap,
      BTreeSet,
   },
   fs::read_to_string,
   io,
};

use mummer::hash::{
   Digest,
   OutputHash,
};
use serde::{
   Deserialize,
   Serialize,
};

use crate::{
   fetch::{
      dispatch::Producer,
      drv::{
         Place,
         Plan,
         Row,
         attr,
         json_file,
         lock_path,
         read_toml,
      },
   },
   nix_store::Store,
};

const PROXY: &str = "https://proxy.golang.org";

#[derive(Deserialize)]
struct Locks {
   go: BTreeMap<String, BTreeMap<String, Digest>>,
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
fn go_sum(text: &str) -> io::Result<Vec<Module>> {
   let mut seen = BTreeSet::new();
   let mut modules = Vec::new();
   for line in text.lines().filter(|line| !line.is_empty()) {
      let mut words = line.split(' ');
      let (Some(path), Some(listed)) = (words.next(), words.next()) else {
         return Err(io::Error::other(format!("goModules: go.sum line {line}")));
      };
      let version = listed.trim_end_matches("/go.mod");
      if seen.insert((path, version)) {
         modules.push(Module {
            path:    path.to_owned(),
            version: version.to_owned(),
         });
      }
   }
   Ok(modules)
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
      let locks: Locks = read_toml(attr("locks")?)?;
      let modules = go_sum(&read_to_string(lock_path("go.sum")?)?)?;
      let mut missing = Vec::new();
      let mut plan = Plan::new("go-modules");
      for module in &modules {
         let (path, version) = (&module.path, &module.version);
         let Some(hashes) = locks.go.get(&format!("{path}@{version}")) else {
            missing.push(format!("{path}@{version}"));
            continue;
         };
         let dir = format!("{}/@v", proxy_case(path));
         for (ext, digest) in hashes {
            let file = format!("{}.{ext}", proxy_case(version));
            plan.fetch(
               Row {
                  file: format!("{path}-{file}"),
                  url:  format!("{PROXY}/{dir}/{file}"),
                  hash: OutputHash::sri(digest.clone()),
               },
               Place::Link(format!("{dir}/{file}")),
            );
         }
         plan.add(json_file(
            &format!("{dir}/{}.info", proxy_case(version)),
            &Info { version },
         )?);
      }
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
      plan.submit(store)
   }
}
