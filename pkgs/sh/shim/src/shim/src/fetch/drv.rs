use std::{
   collections::{
      BTreeMap,
      BTreeSet,
   },
   env::var,
   io::{
      self,
      Write as _,
   },
};

use base64::{
   Engine as _,
   engine::general_purpose::STANDARD,
};
use serde::{
   Deserialize,
   Serialize,
};
use serde_json::{
   Map,
   Value,
   to_value,
};

use crate::nix_store::{
   Derivation,
   FetchFile,
   Output,
   Store,
};

pub fn attr(name: &str) -> io::Result<String> {
   var(name).map_err(|_unset| io::Error::other(format!("{name} is not set")))
}

pub fn optional(name: &str) -> Option<String> {
   var(name).ok().filter(|value| !value.is_empty())
}

/// `<source>/<root>/<file>`, where the producers find their lock.
pub fn lock_path(file: &str) -> io::Result<String> {
   Ok(format!("{}/{}/{file}", attr("source")?, attr("root")?))
}

pub fn say(message: &str) {
   drop(writeln!(io::stderr().lock(), "{message}"));
}

pub enum Hash {
   Sri(String),
   Sha256(String),
}

/// one file to fetch, `file` names it in the store.
pub struct Row {
   pub file: String,
   pub url:  String,
   pub hash: Hash,
}

pub struct Fetched {
   pub drv: String,
   pub out: String,
}

fn parse_sri(text: &str) -> io::Result<(String, String, String)> {
   let sri = text
      .split_whitespace()
      .next()
      .ok_or_else(|| io::Error::other("empty integrity"))?;
   let (algo, encoded) = sri
      .split_once('-')
      .ok_or_else(|| io::Error::other(format!("integrity {sri} is not an SRI hash")))?;
   let bytes = STANDARD
      .decode(encoded)
      .map_err(|error| io::Error::other(format!("integrity {sri}, {error}")))?;
   Ok((algo.to_owned(), hex(&bytes), sri.to_owned()))
}

pub fn hex(bytes: &[u8]) -> String {
   const HEX: &[u8; 16] = b"0123456789abcdef";
   let mut text = String::with_capacity(bytes.len() * 2);
   for byte in bytes {
      text.push(char::from(HEX[usize::from(byte >> 4_u8)]));
      text.push(char::from(HEX[usize::from(byte & 0x0F)]));
   }
   text
}

fn store_name(file: &str) -> String {
   file
      .chars()
      .enumerate()
      .map(|(index, character)| {
         let allowed = character.is_ascii_alphanumeric()
            || matches!(character, '+' | '.' | '_' | '?' | '=' | '-');
         if allowed && !(index == 0 && character == '.') {
            character
         } else {
            '_'
         }
      })
      .collect()
}

/// one builtin:fetchurl derivation per row, in order.
pub fn fetchurls(store: &mut Store, rows: &[Row]) -> io::Result<Vec<Fetched>> {
   let mut files = Vec::with_capacity(rows.len());
   for row in rows {
      let (algo, hex, sri) = match row.hash {
         Hash::Sri(ref text) => parse_sri(text)?,
         Hash::Sha256(ref text) => ("sha256".to_owned(), text.to_lowercase(), text.clone()),
      };
      files.push(FetchFile {
         name: store_name(&row.file),
         url: row.url.clone(),
         algo,
         hex,
         sri,
      });
   }
   Ok(store
      .fetchurls(&files)?
      .into_iter()
      .map(|made| {
         Fetched {
            drv: made.drv,
            out: made.out,
         }
      })
      .collect())
}

/// one thing the collecting derivation lays out under $out at `to`.
#[derive(Deserialize, Serialize)]
pub struct Entry {
   pub to:   String,
   #[serde(flatten)]
   pub kind: Kind,
}

#[derive(Deserialize, Serialize)]
#[serde(untagged)]
pub enum Kind {
   Link { link: String },
   Unpack { unpack: String },
   Write { write: String },
   Copy { copy: String, append: String },
}

impl Entry {
   pub const fn link(link: String, to: String) -> Self {
      Self {
         to,
         kind: Kind::Link { link },
      }
   }

   pub const fn unpack(unpack: String, to: String) -> Self {
      Self {
         to,
         kind: Kind::Unpack { unpack },
      }
   }

   pub const fn write(write: String, to: String) -> Self {
      Self {
         to,
         kind: Kind::Write { write },
      }
   }

   pub const fn copy(copy: String, append: String, to: String) -> Self {
      Self {
         to,
         kind: Kind::Copy { copy, append },
      }
   }
}

pub fn json_file<Data>(to: &str, value: &Data) -> io::Result<Entry>
where
   Data: Serialize,
{
   Ok(Entry::write(
      serde_json::to_string_pretty(value)?,
      to.to_owned(),
   ))
}

fn input_drvs(inputs: &[String]) -> BTreeMap<String, BTreeSet<String>> {
   inputs
      .iter()
      .map(|drv| (drv.clone(), BTreeSet::from(["out".to_owned()])))
      .collect()
}

/// the collecting derivation, submitted as this producer's output, a
/// floating-CA derivation run as `shim assemble` (or `script` with `attrs` as
/// structured attrs). `inputs` are the .drv paths `layout` refers to.
pub fn collect(
   store: &mut Store,
   name: &str,
   layout: &[Entry],
   inputs: &[String],
   script: Option<&str>,
   attrs: Map<String, Value>,
   srcs: &[String],
) -> io::Result<()> {
   let crib = attr("crib")?;
   let shim = attr("shim")?;
   let system = attr("system")?;
   let mut json = Map::new();
   json.insert("name".to_owned(), Value::String(name.to_owned()));
   json.insert("system".to_owned(), Value::String(system.clone()));
   json.insert(
      "outputs".to_owned(),
      Value::Array(vec![Value::String("out".to_owned())]),
   );
   json.insert("crib".to_owned(), Value::String(crib.clone()));
   json.insert("layout".to_owned(), to_value(layout)?);
   json.extend(attrs);
   let mut sources = BTreeSet::from([crib, shim.clone()]);
   sources.extend(srcs.iter().cloned());
   let derivation = Derivation {
      outputs: BTreeMap::from([("out".to_owned(), Output {
         algorithm: "r:sha256".to_owned(),
         ..Output::default()
      })]),
      inputs: input_drvs(inputs),
      sources: sources.clone(),
      system,
      builder: format!("{shim}/bin/shim"),
      args: vec![script.unwrap_or("assemble").to_owned()],
      env: BTreeMap::from([("__json".to_owned(), Value::Object(json).to_string())]),
   };
   let mut references = sources;
   references.extend(inputs.iter().cloned());
   let drv = store.add_derivation(&format!("{name}.drv"), &references, &derivation)?;
   say(&format!(
      "{name}: {} entries, {} inputs -> {drv}",
      layout.len(),
      inputs.len()
   ));
   store.submit(&drv, "out")
}

/// a second producer stage, `shim fetch <kind>` run once `inputs` are built,
/// with `attrs` as the JSON file `stage_attrs` names. What it submits is this
/// producer's result, so nix/fetch.nix unwraps such fetchers with one more
/// outputOf.
pub fn stage<Attrs>(
   store: &mut Store,
   name: &str,
   kind: &str,
   attrs: &Attrs,
   inputs: &[String],
   srcs: &[String],
) -> io::Result<()>
where
   Attrs: Serialize,
{
   let crib = attr("crib")?;
   let shim = attr("shim")?;
   let system = attr("system")?;
   let attrs_file = store.add_text(
      &format!("{name}-attrs.json"),
      &BTreeSet::new(),
      &serde_json::to_vec(attrs)?,
   )?;
   let drv_name = format!("{name}.drv");
   let mut sources = BTreeSet::from([crib.clone(), shim.clone(), attrs_file.clone()]);
   sources.extend(srcs.iter().cloned());
   let env = [
      ("name", drv_name.clone()),
      ("system", system.clone()),
      ("crib", crib),
      ("shim", shim.clone()),
      ("stage_attrs", attrs_file),
      ("requiredSystemFeatures", "builder-rpc-v0".to_owned()),
      ("preferLocalBuild", "1".to_owned()),
      ("outputHashMode", "text".to_owned()),
      ("outputHashAlgo", "sha256".to_owned()),
   ]
   .into_iter()
   .map(|(key, value)| (key.to_owned(), value))
   .collect();
   let derivation = Derivation {
      outputs: BTreeMap::from([("out".to_owned(), Output {
         algorithm: "text:sha256".to_owned(),
         ..Output::default()
      })]),
      inputs: input_drvs(inputs),
      sources: sources.clone(),
      system,
      builder: format!("{shim}/bin/shim"),
      args: vec!["fetch".to_owned(), kind.to_owned()],
      env,
   };
   let mut references = sources;
   references.extend(inputs.iter().cloned());
   let drv = store.add_derivation(&format!("{drv_name}.drv"), &references, &derivation)?;
   say(&format!(
      "{name}: second stage after {} inputs -> {drv}",
      inputs.len()
   ));
   store.submit(&drv, "out")
}
