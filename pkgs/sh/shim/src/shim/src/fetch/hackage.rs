use std::{
   collections::BTreeMap,
   fs::read_to_string,
   io,
};

use serde::Deserialize;
use serde_json::Map;
use shim::hackage::PackageId;

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
         say,
      },
   },
   nix_store::Store,
};

const HACKAGE: &str = "https://hackage.haskell.org/package";

#[derive(Deserialize)]
struct Locks {
   hackage: BTreeMap<PackageId, Locked>,
}

#[derive(Deserialize)]
struct Locked {
   rev:   String,
   src:   String,
   cabal: String,
}

/// locks/hackage.toml -> one file+noindex repository for all cabal packages,
/// <pkg>-<version>.tar.gz with the revised .cabal beside it.
pub struct Hackage;

impl Producer for Hackage {
   fn name(&self) -> &'static str {
      "hackage"
   }

   fn run(&self, store: &mut Store) -> io::Result<()> {
      let locks: Locks =
         toml::from_str(&read_to_string(attr("locks")?)?).map_err(io::Error::other)?;
      let mut rows = Vec::with_capacity(locks.hackage.len() * 2);
      let mut targets = Vec::with_capacity(locks.hackage.len() * 2);
      for (id, entry) in &locks.hackage {
         targets.push(format!("{id}.tar.gz"));
         rows.push(Row {
            file: format!("{id}.tar.gz"),
            url:  format!("{HACKAGE}/{id}/{id}.tar.gz"),
            hash: Hash::Sri(entry.src.clone()),
         });
         targets.push(format!("{id}.cabal"));
         rows.push(Row {
            file: format!("{id}-r{}.cabal", entry.rev),
            url:  format!("{HACKAGE}/{id}/revision/{}.cabal", entry.rev),
            hash: Hash::Sri(entry.cabal.clone()),
         });
      }
      let fetched = fetchurls(store, &rows)?;
      say(&format!("hackageSet: {} packages", locks.hackage.len()));
      let layout: Vec<Entry> = targets
         .iter()
         .zip(&fetched)
         .map(|(to, made)| Entry::link(made.out.clone(), to.clone()))
         .collect();
      let inputs: Vec<String> = fetched.into_iter().map(|made| made.drv).collect();
      collect(store, "hackage-set", &layout, &inputs, None, Map::new(), &[
      ])
   }
}
