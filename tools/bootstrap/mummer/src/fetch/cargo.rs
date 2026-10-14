use std::{
   collections::{
      BTreeMap,
      BTreeSet,
   },
   fs::read_to_string,
   io,
   path::Path,
};

use mummer::{
   hash::{
      Algorithm,
      OutputHash,
   },
   store_path::DrvPath,
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
         reject,
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

/// Vendors the crates of every Cargo.lock in `locks`, relative to the source or
/// absolute, into a directory cargo accepts as [source.vendored-sources], each
/// with its .cargo-checksum.json.
pub struct Cargo;

impl Producer for Cargo {
   fn name(&self) -> &'static str {
      "cargo"
   }

   fn run(&self, store: &mut Store) -> io::Result<()> {
      let source = attr("source")?;
      let locks = attr("locks")?
         .lines()
         .map(|lock| read_to_string(Path::new(&source).join(lock)))
         .collect::<io::Result<Vec<String>>>()?;
      let vendor = vendor_layout(store, &locks, "")?;

      collect(store, "cargo-vendor", &vendor.layout, &vendor.drvs)
   }
}

pub struct Vendor {
   pub layout: Vec<Entry>,
   pub drvs:   Vec<DrvPath>,
}

pub fn vendor_layout(store: &mut Store, locks: &[String], prefix: &str) -> io::Result<Vendor> {
   let mut listed = Vec::new();

   for text in locks {
      listed.extend(
         toml::from_str::<Lock>(text)
            .map_err(io::Error::other)?
            .package,
      );
   }

   let mut seen = BTreeSet::new();
   let packages = listed
      .iter()
      .filter(|package| seen.insert((package.name.as_str(), package.version.as_str())))
      .collect::<Vec<&Package>>();

   let foreign = packages
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
      .collect::<Vec<String>>();
   reject("cargoVendor: unsupported sources", &foreign)?;
   let mut crates = Vec::new();
   let mut rows = Vec::new();
   for &package in &packages {
      if package.source.as_deref() != Some(CRATES_IO) {
         continue;
      }
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
      crates.push((package, checksum));
   }
   let fetched = fetchurls(store, &rows)?;
   let mut layout = Vec::with_capacity(crates.len() * 2);
   for (&(package, checksum), made) in crates.iter().zip(&fetched) {
      let dir = format!("{prefix}{}-{}", package.name, package.version);
      layout.push(Entry::unpack(made.out.to_string(), dir.clone()));
      layout.push(json_file(
         &format!("{dir}/.cargo-checksum.json"),
         &Checksum {
            files:   BTreeMap::new(),
            package: checksum,
         },
      )?);
   }
   let drvs = fetched.into_iter().map(|made| made.drv).collect();
   Ok(Vendor { layout, drvs })
}
