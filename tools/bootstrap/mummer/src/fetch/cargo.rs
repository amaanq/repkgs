use std::{
   collections::BTreeMap,
   fs::read_to_string,
   io,
};

use mummer::hash::{
   Algorithm,
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
         Entry,
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
      let vendor = vendor_layout(store, &read_to_string(format!("{source}/Cargo.lock"))?, "")?;
      collect(
         store,
         "cargo-vendor",
         &vendor.layout,
         &vendor.drvs,
         None,
         serde_json::Map::new(),
         &[],
      )
   }
}

pub struct Vendor {
   pub layout: Vec<Entry>,
   pub drvs:   Vec<String>,
}

pub fn vendor_layout(store: &mut Store, text: &str, prefix: &str) -> io::Result<Vendor> {
   let lock: Lock = toml::from_str(text).map_err(io::Error::other)?;
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
         hash: OutputHash::hex(Algorithm::Sha256, checksum)?,
      });
   }
   let fetched = fetchurls(store, &rows)?;
   let mut layout = Vec::with_capacity(crates.len() * 2);
   for (package, made) in crates.iter().zip(&fetched) {
      let dir = format!("{prefix}{}-{}", package.name, package.version);
      layout.push(Entry::unpack(made.out.clone(), dir.clone()));
      layout.push(json_file(
         &format!("{dir}/.cargo-checksum.json"),
         &Checksum {
            files:   BTreeMap::new(),
            package: package.checksum.as_deref().unwrap_or_default(),
         },
      )?);
   }
   let drvs: Vec<String> = fetched.into_iter().map(|made| made.drv).collect();
   Ok(Vendor { layout, drvs })
}
