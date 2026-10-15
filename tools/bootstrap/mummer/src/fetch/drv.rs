use std::{
   collections::{
      BTreeMap,
      BTreeSet,
   },
   env::var,
   fmt::{
      Display,
      Formatter,
      Result as FormatResult,
   },
   fs::read_to_string,
   io::{
      self,
      Write as _,
   },
   path::Path,
};

use mummer::{
   hash::OutputHash,
   store_path::{
      DrvPath,
      StorePath,
   },
};
use serde::{
   Deserialize,
   Deserializer,
   Serialize,
   de::{
      DeserializeOwned,
      IgnoredAny,
      MapAccess,
      Visitor,
   },
};

use crate::nix_store::{
   Derivation,
   FetchFile,
   Floating,
   Output,
   Realisation,
   Store,
};

pub fn attr(name: &str) -> io::Result<String> {
   var(name).map_err(|_unset| io::Error::other(format!("{name} is not set")))
}

pub fn store_attr(name: &str) -> io::Result<StorePath> {
   attr(name)?.try_into()
}

pub fn optional(name: &str) -> Option<String> {
   var(name).ok().filter(|value| !value.is_empty())
}

/// `<source>/<root>`, where the producers find their lock.
pub fn lock_dir() -> io::Result<String> {
   Ok(format!("{}/{}", attr("source")?, attr("root")?))
}

/// `lockFile` when the producer was handed one apart from the source, else
/// `file` in the lock directory.
pub fn lock_path(file: &str) -> io::Result<String> {
   optional("lockFile").map_or_else(|| Ok(format!("{}/{file}", lock_dir()?)), Ok)
}

pub fn read_toml<Data, File>(path: File) -> io::Result<Data>
where
   Data: DeserializeOwned,
   File: AsRef<Path>,
{
   toml::from_str(&read_to_string(path)?).map_err(io::Error::other)
}

/// An error naming every one of `names` after `message`, when there are any.
pub fn reject<Name>(message: &str, names: &[Name]) -> io::Result<()>
where
   Name: Display,
{
   if names.is_empty() {
      return Ok(());
   }
   let listed = names
      .iter()
      .map(ToString::to_string)
      .collect::<Vec<_>>()
      .join(", ");
   Err(io::Error::other(format!("{message}: {listed}")))
}

/// A JSON object's keys, its values unread.
pub fn keys<'de, Input>(input: Input) -> Result<BTreeSet<String>, Input::Error>
where
   Input: Deserializer<'de>,
{
   input.deserialize_map(Keys)
}

struct Keys;

impl<'de> Visitor<'de> for Keys {
   type Value = BTreeSet<String>;

   fn expecting(&self, formatter: &mut Formatter<'_>) -> FormatResult {
      formatter.write_str("an object")
   }

   fn visit_map<Access>(self, mut map: Access) -> Result<Self::Value, Access::Error>
   where
      Access: MapAccess<'de>,
   {
      let mut found = BTreeSet::new();
      while let Some(key) = map.next_key::<String>()? {
         map.next_value::<IgnoredAny>()?;
         found.insert(key);
      }
      Ok(found)
   }
}

pub fn say(message: &str) {
   drop(writeln!(io::stderr().lock(), "{message}"));
}

/// one file to fetch, `file` names it in the store.
pub struct Row {
   pub file: String,
   pub url:  String,
   pub hash: OutputHash,
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
pub fn fetchurls(store: &mut Store, rows: &[Row]) -> io::Result<Vec<Realisation>> {
   let files = rows
      .iter()
      .map(|row| {
         FetchFile {
            name: store_name(&row.file),
            url:  row.url.clone(),
            hash: row.hash.clone(),
         }
      })
      .collect::<Vec<_>>();
   store.fetchurls(&files)
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
   Zip { zip: Vec<Member> },
}

/// A file a `Zip` entry stores, at `name` inside the archive.
#[derive(Deserialize, Serialize)]
pub struct Member {
   pub name: String,
   pub path: String,
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

   pub const fn zip(zip: Vec<Member>, to: String) -> Self {
      Self {
         to,
         kind: Kind::Zip { zip },
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

/// Where a fetched file goes under the collecting derivation's `$out`.
pub enum Place {
   Link(String),
   Unpack(String),
}

/// What a lock-file producer fetches and lays out, each fetch at its place in
/// order, then `entries`.
pub struct Plan {
   name:    &'static str,
   fetches: Vec<(Row, Place)>,
   entries: Vec<Entry>,
}

impl Plan {
   pub const fn new(name: &'static str) -> Self {
      Self {
         name,
         fetches: Vec::new(),
         entries: Vec::new(),
      }
   }

   pub fn fetch(&mut self, row: Row, place: Place) {
      self.fetches.push((row, place));
   }

   pub fn add(&mut self, entry: Entry) {
      self.entries.push(entry);
   }

   pub fn submit(self, store: &mut Store) -> io::Result<()> {
      let (rows, places) = self.fetches.into_iter().unzip::<_, _, Vec<_>, Vec<_>>();
      let fetched = fetchurls(store, &rows)?;
      let mut layout = places
         .into_iter()
         .zip(&fetched)
         .map(|(place, made)| {
            match place {
               Place::Link(to) => Entry::link(made.out.to_string(), to),
               Place::Unpack(to) => Entry::unpack(made.out.to_string(), to),
            }
         })
         .collect::<Vec<_>>();
      layout.extend(self.entries);
      let inputs = fetched.into_iter().map(|made| made.drv).collect::<Vec<_>>();
      collect(store, self.name, &layout, &inputs)
   }
}

/// The store paths a producer's derivations run and the platform they run on.
struct Toolchain {
   peanut: StorePath,
   mummer: StorePath,
   system: String,
}

impl Toolchain {
   fn load() -> io::Result<Self> {
      Ok(Self {
         peanut: store_attr("peanut")?,
         mummer: store_attr("mummer")?,
         system: attr("system")?,
      })
   }

   /// `job` added as a derivation reading `inputs` and submitted as this
   /// producer's output.
   fn submit(&self, store: &mut Store, job: Job<'_>, inputs: &[DrvPath]) -> io::Result<DrvPath> {
      let mut references = job.sources.clone();
      references.extend(inputs.iter().cloned().map(StorePath::from));
      let derivation = Derivation {
         outputs: BTreeMap::from([("out".to_owned(), Output::floating(job.output))]),
         inputs:  inputs
            .iter()
            .map(|drv| (drv.clone(), BTreeSet::from(["out".to_owned()])))
            .collect(),
         sources: job.sources,
         system:  self.system.clone(),
         builder: format!("{}/bin/mummer", self.mummer),
         args:    job.args,
         env:     job.env,
      };
      let drv = store.add_derivation(&format!("{}.drv", job.name), &references, &derivation)?;
      store.submit(&drv, "out")?;
      Ok(drv)
   }
}

/// A derivation run as `mummer <args>`, its one output floating.
struct Job<'name> {
   name:    &'name str,
   output:  Floating,
   args:    Vec<String>,
   env:     BTreeMap<String, String>,
   sources: BTreeSet<StorePath>,
}

/// The `__json` of a collecting derivation, `attrs` after the fields every one
/// has.
#[derive(Serialize)]
struct Collecting<'layout, Attrs> {
   name:    &'layout str,
   system:  &'layout str,
   outputs: [&'static str; 1],
   peanut:  &'layout StorePath,
   layout:  &'layout [Entry],
   #[serde(flatten)]
   attrs:   Attrs,
}

/// How a collecting derivation assembles, `mummer <script>` with `attrs`
/// beside the layout and `srcs` as further sources.
pub struct Assembly<'srcs, Attrs> {
   pub script: &'static str,
   pub attrs:  Attrs,
   pub srcs:   &'srcs [StorePath],
}

#[derive(Serialize)]
pub struct NoAttrs;

/// The collecting derivation, submitted as this producer's output, a
/// floating-CA derivation run as `mummer assemble`. `inputs` are the .drv
/// paths `layout` refers to.
pub fn collect(
   store: &mut Store,
   name: &str,
   layout: &[Entry],
   inputs: &[DrvPath],
) -> io::Result<()> {
   collect_with(store, name, layout, inputs, Assembly {
      script: "assemble",
      attrs:  NoAttrs,
      srcs:   &[],
   })
}

pub fn collect_with<Attrs>(
   store: &mut Store,
   name: &str,
   layout: &[Entry],
   inputs: &[DrvPath],
   assembly: Assembly<'_, Attrs>,
) -> io::Result<()>
where
   Attrs: Serialize,
{
   let toolchain = Toolchain::load()?;
   let json = serde_json::to_string(&Collecting {
      name,
      system: &toolchain.system,
      outputs: ["out"],
      peanut: &toolchain.peanut,
      layout,
      attrs: assembly.attrs,
   })?;
   let mut sources = BTreeSet::from([toolchain.peanut.clone(), toolchain.mummer.clone()]);
   sources.extend(assembly.srcs.iter().cloned());
   let drv = toolchain.submit(
      store,
      Job {
         name,
         output: Floating::Recursive,
         args: vec![assembly.script.to_owned()],
         env: BTreeMap::from([("__json".to_owned(), json)]),
         sources,
      },
      inputs,
   )?;
   say(&format!(
      "{name}: {} entries, {} inputs -> {drv}",
      layout.len(),
      inputs.len()
   ));
   Ok(())
}

/// A second producer stage, `mummer fetch <kind>` run once `inputs` are built,
/// with `attrs` as the JSON file `stage_attrs` names. What it submits is this
/// producer's result, so nix/fetch.nix unwraps such fetchers with one more
/// outputOf.
pub fn stage<Attrs>(
   store: &mut Store,
   name: &str,
   kind: &str,
   attrs: &Attrs,
   inputs: &[DrvPath],
   srcs: &[StorePath],
) -> io::Result<()>
where
   Attrs: Serialize,
{
   let toolchain = Toolchain::load()?;
   let attrs_file = store.add_text(
      &format!("{name}-attrs.json"),
      &BTreeSet::new(),
      &serde_json::to_vec(attrs)?,
   )?;
   let drv_name = format!("{name}.drv");
   let env = [
      ("name", drv_name.clone()),
      ("system", toolchain.system.clone()),
      ("peanut", toolchain.peanut.to_string()),
      ("mummer", toolchain.mummer.to_string()),
      ("stage_attrs", attrs_file.to_string()),
      ("requiredSystemFeatures", "builder-rpc-v0".to_owned()),
      ("preferLocalBuild", "1".to_owned()),
      ("outputHashMode", "text".to_owned()),
      ("outputHashAlgo", "sha256".to_owned()),
   ]
   .into_iter()
   .map(|(key, value)| (key.to_owned(), value))
   .collect();
   let mut sources = BTreeSet::from([
      toolchain.peanut.clone(),
      toolchain.mummer.clone(),
      attrs_file,
   ]);
   sources.extend(srcs.iter().cloned());
   let drv = toolchain.submit(
      store,
      Job {
         name: &drv_name,
         output: Floating::Text,
         args: vec!["fetch".to_owned(), kind.to_owned()],
         env,
         sources,
      },
      inputs,
   )?;
   say(&format!(
      "{name}: second stage after {} inputs -> {drv}",
      inputs.len()
   ));
   Ok(())
}
