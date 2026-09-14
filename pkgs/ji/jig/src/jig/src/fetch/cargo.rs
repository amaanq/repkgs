use std::{
   collections::BTreeMap,
   fs::read_to_string,
   io,
};

use serde::{
   Deserialize,
   Serialize,
};

use crate::{
   fetch::{
      dispatch::Producer,
      drv::{
         Entry,
         Hash,
         Row,
         attr,
         collect,
         fetchurls,
         json_file,
      },
   },
   nix_store::Store,
};

const CRATES_IO: &str = "registry+https://github.com/rust-lang/crates.io-index";

#[derive(Deserialize)]
struct Lock {
   package: Vec<Package>,
}

#[derive(Deserialize)]
struct Package {
   name:     String,
   version:  String,
   source:   Option<String>,
   checksum: Option<String>,
}

#[derive(Serialize)]
struct Checksum<'package> {
   files:   BTreeMap<String, String>,
   package: &'package str,
}

/// Cargo.lock -> a directory cargo accepts as [source.vendored-sources],
/// crates unpacked with their .cargo-checksum.json.
pub struct Cargo;

impl Producer for Cargo {
   fn name(&self) -> &'static str {
      "cargo"
   }

   fn run(&self, store: &mut Store) -> io::Result<()> {
      let source = attr("source")?;
      let lock: Lock = toml::from_str(&read_to_string(format!("{source}/Cargo.lock"))?)
         .map_err(io::Error::other)?;
      let foreign: Vec<String> = lock
         .package
         .iter()
         .filter(|package| {
            package
               .source
               .as_deref()
               .is_some_and(|origin| origin != CRATES_IO)
         })
         .map(|package| {
            format!(
               "{} {}",
               package.name,
               package.source.as_deref().unwrap_or_default()
            )
         })
         .collect();
      if !foreign.is_empty() {
         return Err(io::Error::other(format!(
            "cargoVendor: unsupported sources: {}",
            foreign.join(", ")
         )));
      }
      let crates: Vec<&Package> = lock
         .package
         .iter()
         .filter(|package| package.source.as_deref() == Some(CRATES_IO))
         .collect();
      let mut rows = Vec::with_capacity(crates.len());
      for package in &crates {
         let checksum = package.checksum.as_deref().ok_or_else(|| {
            io::Error::other(format!(
               "cargoVendor: {} {} has no checksum",
               package.name, package.version
            ))
         })?;
         rows.push(Row {
            file: format!("{}-{}.tar.gz", package.name, package.version),
            url:  format!(
               "https://static.crates.io/crates/{}/{}-{}.crate",
               package.name, package.name, package.version
            ),
            hash: Hash::Sha256(checksum.to_owned()),
         });
      }
      let fetched = fetchurls(store, &rows)?;
      let mut layout = Vec::with_capacity(crates.len() * 2);
      for (package, made) in crates.iter().zip(&fetched) {
         let dir = format!("{}-{}", package.name, package.version);
         layout.push(Entry::unpack(made.out.clone(), dir.clone()));
         layout.push(json_file(
            &format!("{dir}/.cargo-checksum.json"),
            &Checksum {
               files:   BTreeMap::new(),
               package: package.checksum.as_deref().unwrap_or_default(),
            },
         )?);
      }
      let inputs: Vec<String> = fetched.into_iter().map(|made| made.drv).collect();
      collect(
         store,
         "cargo-vendor",
         &layout,
         &inputs,
         None,
         serde_json::Map::new(),
      )
   }
}
