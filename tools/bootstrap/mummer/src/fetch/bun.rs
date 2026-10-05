use std::{
   collections::{
      BTreeMap,
      BTreeSet,
   },
   fs::read_to_string,
   io,
};

use mummer::hash::OutputHash;
use serde::{
   Deserialize,
   Serialize,
};
use serde_json::Value;

use crate::{
   fetch::{
      dispatch::Producer,
      drv::{
         Place,
         Plan,
         Row,
         json_file,
         lock_path,
         reject,
      },
      registry::PackageId,
   },
   nix_store::Store,
};

#[derive(Deserialize)]
struct Lock {
   packages: BTreeMap<String, Package>,
}

/// `[id, registry, info, integrity]`, a workspace or git package without the
/// integrity bun writes last for registry tarballs.
#[derive(Deserialize)]
#[serde(try_from = "Vec<Value>")]
struct Package {
   id:        String,
   registry:  Option<String>,
   integrity: Option<String>,
}

impl TryFrom<Vec<Value>> for Package {
   type Error = io::Error;

   fn try_from(fields: Vec<Value>) -> io::Result<Self> {
      let text = |field: Option<&Value>| field.and_then(Value::as_str).map(str::to_owned);
      Ok(Self {
         id:        text(fields.first())
            .ok_or_else(|| io::Error::other("bunDeps: a bun.lock package has no id"))?,
         registry:  text(fields.get(1)).filter(|registry| !registry.is_empty()),
         integrity: text(fields.last()).filter(|last| last.starts_with("sha512-")),
      })
   }
}

#[derive(Serialize)]
struct Indexed<'entry> {
   id:  &'entry str,
   dir: String,
}

/// bun writes trailing commas but no comments.
fn strip_trailing_commas(text: &str) -> String {
   let mut cleaned = String::with_capacity(text.len());
   let mut in_string = false;
   let mut escaped = false;
   let mut pending_comma = false;
   let mut held = String::new();
   for character in text.chars() {
      if in_string {
         cleaned.push(character);
         if escaped {
            escaped = false;
         } else if character == '\\' {
            escaped = true;
         } else if character == '"' {
            in_string = false;
         }
         continue;
      }
      if pending_comma {
         if character.is_whitespace() {
            held.push(character);
            continue;
         }
         if character != '}' && character != ']' {
            cleaned.push(',');
         }
         cleaned.push_str(&held);
         held.clear();
         pending_comma = false;
      }
      match character {
         ',' => pending_comma = true,
         '"' => {
            in_string = true;
            cleaned.push(character);
         },
         _ => cleaned.push(character),
      }
   }
   if pending_comma {
      cleaned.push(',');
      cleaned.push_str(&held);
   }
   cleaned
}

/// bun.lock -> { p/<name>@<version>/ (unpacked), index.json }, linked into
/// `$BUN_INSTALL_CACHE_DIR` under the names bun expects.
pub struct Bun;

impl Producer for Bun {
   fn name(&self) -> &'static str {
      "bun"
   }

   fn run(&self, store: &mut Store) -> io::Result<()> {
      let text = read_to_string(lock_path("bun.lock")?)?;
      let lock = serde_json::from_str::<Lock>(&strip_trailing_commas(&text))?;
      let mut unhashed = Vec::new();
      let mut plan = Plan::new("bun-deps");
      let mut seen = BTreeSet::new();
      let mut index = Vec::new();
      for package in lock.packages.values() {
         let Some(ref integrity) = package.integrity else {
            if !package.id.contains("@workspace:") {
               unhashed.push(package.id.as_str());
            }
            continue;
         };
         if !seen.insert(package.id.as_str()) {
            continue;
         }
         let id = PackageId::try_from(package.id.as_str())?;
         let dir = format!("p/{}@{}", id.flat_name(), id.version);
         plan.fetch(
            Row {
               file: format!("{}-{}.tgz", id.flat_name(), id.version),
               url:  id.tarball_url(package.registry.as_deref()),
               hash: OutputHash::integrity(integrity)?,
            },
            Place::Unpack(dir.clone()),
         );
         index.push(Indexed {
            id: &package.id,
            dir,
         });
      }
      reject(
         "bunDeps: bun.lock has no integrity (git/github/file dependencies) for",
         &unhashed,
      )?;
      plan.add(json_file("index.json", &index)?);
      plan.submit(store)
   }
}
