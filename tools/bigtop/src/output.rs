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
use serde_json::Value;
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
/// Builds of one package kept apart, other pins or platforms.
const BUILDS: usize = 4;
/// How many a list holds at most, when the cache keeps entries past the
/// limit because they were read recently.
const SPARE: usize = 4;

#[derive(Serialize)]
struct Record<'inputs> {
   attrs:      &'inputs str,
   toolchains: Vec<String>,
   shared:     &'static str,
}

/// A build of a package by its key, and by what decides it besides the
/// package's own sources, so a build differing only in those replaces it.
#[derive(Deserialize, Serialize)]
struct Build {
   digest:  String,
   context: String,
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
      ("dev", attrs.outputs.dev.clone()),
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
   identity(attrs, outputs).map(|found| found.0)
}

/// The output key's digest, and the digest of the same inputs without the
/// package's sources, its version and its lock trees.
fn identity(attrs: &Attrs, outputs: &[Output]) -> io::Result<(String, String)> {
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
   let mut sourceless = serde_json::from_str::<Value>(&text)?;
   if let Some(fields) = sourceless.as_object_mut() {
      fields.remove("src");
      fields.remove("patches");
      if let Some(spec) = fields.get_mut("spec").and_then(Value::as_object_mut) {
         spec.remove("version");
      }
   }
   let mut context_text = serde_json::to_string(&sourceless)?;
   for tree in attrs.spec()?.lock_trees() {
      context_text = context_text.replace(tree, "<lock>");
   }
   let digest = |attrs_text: &str| -> io::Result<String> {
      let record = Record {
         attrs:      attrs_text,
         toolchains: toolchains.clone(),
         shared:     SHARED,
      };
      Ok(blake3::hash(&serde_json::to_vec(&record)?)
         .to_hex()
         .to_string())
   };
   Ok((digest(&text)?, digest(&context_text)?))
}

fn variants(attrs: &Attrs, digest: &str) -> io::Result<Key> {
   format!("output/{}/{digest}", attrs.spec()?.name).parse()
}

fn entry(attrs: &Attrs, digest: &str, id: &str, output: &Output) -> io::Result<Key> {
   entry_key(attrs, digest, id, output.name)
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
   let (digest, context) = identity(attrs, &outputs)?;
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
   let mut kept = vec![variant];
   if known.len() >= VARIANTS {
      let firsts = known
         .iter()
         .map(|other| entry_key(attrs, &digest, &other.id(&digest)?, "out"))
         .collect::<io::Result<Vec<_>>>()?;
      let mut ranked = known
         .into_iter()
         .zip(client.ages(&firsts)?)
         .filter_map(|(other, age)| Some((age?, other)))
         .collect::<Vec<_>>();
      ranked.sort_by_key(|pair| pair.0);
      let overflow = ranked.split_off(ranked.len().min(VARIANTS - 1));
      known = ranked.into_iter().map(|pair| pair.1).collect();
      for (_, other) in overflow {
         let entries = entry_keys(attrs, &digest, &other.id(&digest)?)?;
         if client.expire(&entries)?.contains(&false) {
            known.push(other);
         }
      }
   }
   kept.extend(known);
   kept.truncate(VARIANTS * SPARE);
   client.put(&listing, &serde_json::to_vec(&kept)?)?;
   lineage(&mut client, attrs, digest, context)
}

fn entry_key(attrs: &Attrs, digest: &str, id: &str, name: &str) -> io::Result<Key> {
   format!("output/{}/{digest}/{id}/{name}", attrs.spec()?.name).parse()
}

fn entry_keys(attrs: &Attrs, digest: &str, id: &str) -> io::Result<Vec<Key>> {
   ["out", "debug", "tree"]
      .into_iter()
      .map(|name| entry_key(attrs, digest, id, name))
      .collect()
}

/// Drops a build's outputs and then its variant list, true when all of it
/// went, `forget` for a build this one replaces, else `expire`.
fn drop_build(
   client: &mut Client,
   attrs: &Attrs,
   digest: &str,
   replaced: bool,
) -> io::Result<bool> {
   let listing = variants(attrs, digest)?;
   let known = client
      .get(&listing)?
      .and_then(|bytes| serde_json::from_slice::<Vec<Variant>>(&bytes).ok())
      .unwrap_or_default();
   let mut keys = Vec::new();
   for variant in &known {
      keys.extend(entry_keys(attrs, digest, &variant.id(digest)?)?);
   }
   let mut drop_keys = |batch: &[Key]| {
      if replaced {
         client.forget(batch)
      } else {
         client.expire(batch)
      }
   };
   if drop_keys(&keys)?.contains(&false) {
      return Ok(false);
   }
   Ok(!drop_keys(&[listing])?.contains(&false))
}

/// The package's recent builds, newest first. A build whose inputs differ
/// from this one only in the package's own sources is replaced and dropped,
/// past `BUILDS` the least recently read go unless read lately.
fn lineage(client: &mut Client, attrs: &Attrs, digest: String, context: String) -> io::Result<()> {
   let key = format!("output/{}", attrs.spec()?.name).parse::<Key>()?;
   let known = client
      .get(&key)?
      .and_then(|bytes| serde_json::from_slice::<Vec<Build>>(&bytes).ok())
      .unwrap_or_default();
   let mut kept = Vec::new();
   for build in known.into_iter().filter(|build| build.digest != digest) {
      if build.context != context || !drop_build(client, attrs, &build.digest, true)? {
         kept.push(build);
      }
   }
   if kept.len() >= BUILDS {
      let listings = kept
         .iter()
         .map(|build| variants(attrs, &build.digest))
         .collect::<io::Result<Vec<_>>>()?;
      let mut ranked = kept
         .into_iter()
         .zip(client.ages(&listings)?)
         .filter_map(|(build, age)| Some((age?, build)))
         .collect::<Vec<_>>();
      ranked.sort_by_key(|pair| pair.0);
      let overflow = ranked.split_off(ranked.len().min(BUILDS - 1));
      kept = ranked.into_iter().map(|pair| pair.1).collect();
      for (_, build) in overflow {
         if !drop_build(client, attrs, &build.digest, false)? {
            kept.push(build);
         }
      }
   }
   let mut builds = vec![Build { digest, context }];
   builds.extend(kept);
   builds.truncate(BUILDS * SPARE);
   client.put(&key, &serde_json::to_vec(&builds)?)
}
