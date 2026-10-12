//! Whole package outputs kept in mummerd, keyed on everything that decides
//! them except the mummer and plumbing binaries, so a change to how builds
//! are cached does not rebuild every package. Of bigtop the key holds the
//! code every build runs, and each entry records the build-system modules
//! its build dispatched into, so an edit to one system rebuilds only the
//! packages that use it.

use std::{
   collections::{
      BTreeMap,
      BTreeSet,
   },
   env::var,
   fs::{
      OpenOptions,
      read_to_string,
      remove_dir_all,
   },
   io::{
      self,
      ErrorKind,
      Write as _,
   },
   path::Path,
};

use memchr::memmem;
use mummer::{
   directory::{
      get_tree,
      put_tree,
   },
   process::socket,
   store::STORE_DIR,
   tree::note,
};
use serde::{
   Deserialize,
   Serialize,
};
use tightrope::{
   client::Client,
   key::Key,
};

use crate::{
   attrs::{
      Attrs,
      build_top,
   },
   probe::toolchain,
};

include!(concat!(env!("OUT_DIR"), "/fingerprints.rs"));

const LEDGER: &str = ".bigtop-modules";
const VARIANTS: usize = 4;

#[derive(Serialize)]
struct Record<'inputs> {
   attrs:      &'inputs str,
   toolchains: Vec<String>,
   shared:     &'static str,
}

/// The build-system modules one build dispatched into, by fingerprint.
#[derive(Deserialize, Serialize, PartialEq, Eq)]
struct Variant {
   modules: BTreeMap<String, String>,
}

impl Variant {
   fn changed(&self) -> Vec<&str> {
      self
         .modules
         .iter()
         .filter(|&(name, hash)| fingerprint(name) != Some(hash.as_str()))
         .map(|(name, _)| name.as_str())
         .collect()
   }

   fn id(&self, digest: &str) -> io::Result<String> {
      let mut hasher = blake3::Hasher::new();
      hasher.update(digest.as_bytes());
      hasher.update(&serde_json::to_vec(&self.modules)?);
      Ok(hasher.finalize().to_hex().to_string())
   }
}

pub fn fingerprint(name: &str) -> Option<&'static str> {
   MODULES
      .iter()
      .find(|entry| entry.0 == name)
      .map(|entry| entry.1)
}

/// Notes that this build dispatched into the build-system module at `path`.
pub fn used(path: &str) -> io::Result<()> {
   let name = path.rsplit("::").next().unwrap_or(path);
   let ledger = Path::new(&build_top()?).join(LEDGER);
   let mut file = OpenOptions::new().create(true).append(true).open(ledger)?;
   writeln!(file, "{name}")
}

fn recorded() -> io::Result<Variant> {
   let ledger = Path::new(&build_top()?).join(LEDGER);
   let text = match read_to_string(ledger) {
      Ok(text) => text,
      Err(error) if error.kind() == ErrorKind::NotFound => String::new(),
      Err(error) => return Err(error),
   };
   let modules = text
      .lines()
      .collect::<BTreeSet<_>>()
      .into_iter()
      .map(|name| {
         let hash = fingerprint(name).ok_or_else(|| {
            io::Error::other(format!("bigtop has no fingerprint for the {name} module"))
         })?;
         Ok((name.to_owned(), hash.to_owned()))
      })
      .collect::<io::Result<_>>()?;
   Ok(Variant { modules })
}

/// An output's path and the stand-in for its hash while stored, the same
/// length so binaries keep their layout, with an `e`, which no store hash has.
struct Output {
   name:  &'static str,
   path:  String,
   token: String,
}

fn outputs(attrs: &Attrs) -> Vec<Output> {
   [
      ("out", Some(attrs.outputs.out.clone())),
      ("debug", attrs.outputs.debug.clone()),
      ("tree", attrs.outputs.tree.clone()),
   ]
   .into_iter()
   .enumerate()
   .filter_map(|(index, (name, path))| {
      Some(Output {
         name,
         path: path?,
         token: format!("{}{index}", "e".repeat(31)),
      })
   })
   .collect()
}

fn hash_part(path: &str) -> &str {
   path
      .strip_prefix(STORE_DIR)
      .and_then(|rest| rest.strip_prefix('/'))
      .and_then(|rest| rest.get(..32))
      .unwrap_or_default()
}

fn swap(mut data: Vec<u8>, pairs: &[(String, String)]) -> Vec<u8> {
   for pair in pairs.iter().filter(|pair| pair.0.len() == pair.1.len()) {
      let found = memmem::find_iter(&data, pair.0.as_bytes()).collect::<Vec<usize>>();
      for position in found {
         data[position..position + pair.1.len()].copy_from_slice(pair.1.as_bytes());
      }
   }
   data
}

fn swap_text(text: String, pairs: &[(String, String)]) -> String {
   pairs
      .iter()
      .fold(text, |current, pair| current.replace(&pair.0, &pair.1))
}

fn key(attrs: &Attrs, outputs: &[Output]) -> io::Result<String> {
   let file = var("NIX_ATTRS_JSON_FILE")
      .map_err(|_unset| io::Error::other("NIX_ATTRS_JSON_FILE is not set"))?;
   let mut text = read_to_string(file)?;
   for output in outputs {
      text = text.replace(&output.path, &format!("<{}>", output.name));
   }
   for helper in &attrs.plumbing {
      text = text.replace(helper.as_str(), "<plumbing>");
   }
   let top = build_top()?;
   let mut toolchains = Vec::new();
   for root in &attrs.build_dependencies {
      if Path::new(root).join("etc/mummer.json").exists() {
         toolchains.push(toolchain(root, &top)?);
         text = text.replace(root.as_str(), "<cc>");
      }
   }
   let record = Record {
      attrs: &text,
      toolchains,
      shared: SHARED,
   };
   Ok(blake3::hash(&serde_json::to_vec(&record)?)
      .to_hex()
      .to_string())
}

fn variants(attrs: &Attrs, digest: &str) -> io::Result<Key> {
   format!("output/{}/{digest}", attrs.spec()?.name).parse()
}

fn entry(attrs: &Attrs, digest: &str, id: &str, output: &Output) -> io::Result<Key> {
   format!(
      "output/{}/{digest}/{id}/{}",
      attrs.spec()?.name,
      output.name
   )
   .parse()
}

/// The outputs a build with these inputs stored, false when any is missing,
/// with nothing left behind.
pub fn restore(attrs: &Attrs) -> io::Result<bool> {
   let Ok(mut client) = Client::connect(&socket()) else {
      return Ok(false);
   };
   let outputs = outputs(attrs);
   let digest = key(attrs, &outputs)?;
   let Some(listed) = client.get(&variants(attrs, &digest)?)? else {
      return Ok(false);
   };
   let known = serde_json::from_slice::<Vec<Variant>>(&listed)?;
   let Some(variant) = known.iter().find(|variant| variant.changed().is_empty()) else {
      if let Some(newest) = known.first() {
         note(
            "output-cache",
            &format!("cold, {} changed", newest.changed().join(" ")),
         );
      }
      return Ok(false);
   };
   let id = variant.id(&digest)?;
   let pairs = outputs
      .iter()
      .map(|output| (output.token.clone(), hash_part(&output.path).to_owned()))
      .collect::<Vec<_>>();
   for (index, output) in outputs.iter().enumerate() {
      let restored = get_tree(
         &mut client,
         &entry(attrs, &digest, &id, output)?,
         Path::new(&output.path),
         |data| swap(data, &pairs),
         |target| swap_text(target, &pairs),
      );
      if !matches!(restored, Ok(true)) {
         for earlier in &outputs[..index] {
            remove_dir_all(&earlier.path)?;
         }
         if let Err(error) = restored {
            note("output-cache", &format!("restore failed with {error}"));
         }
         return Ok(false);
      }
   }
   note("output-cache", "restored");
   Ok(true)
}

pub fn store(attrs: &Attrs) -> io::Result<()> {
   let Ok(mut client) = Client::connect(&socket()) else {
      return Ok(());
   };
   let outputs = outputs(attrs);
   let digest = key(attrs, &outputs)?;
   let variant = recorded()?;
   let id = variant.id(&digest)?;
   let pairs = outputs
      .iter()
      .map(|output| (hash_part(&output.path).to_owned(), output.token.clone()))
      .collect::<Vec<_>>();
   for output in &outputs {
      if !Path::new(&output.path).exists() {
         continue;
      }
      put_tree(
         &mut client,
         &entry(attrs, &digest, &id, output)?,
         Path::new(&output.path),
         |data| swap(data, &pairs),
         |target| swap_text(target, &pairs),
      )?;
   }

   let listing = variants(attrs, &digest)?;
   let mut known = match client.get(&listing)? {
      Some(bytes) => serde_json::from_slice::<Vec<Variant>>(&bytes)?,
      None => Vec::new(),
   };
   known.retain(|other| *other != variant);
   known.insert(0, variant);
   known.truncate(VARIANTS);
   client.put(&listing, &serde_json::to_vec(&known)?)
}
