use std::{
   collections::BTreeMap,
   io,
};

use mummer::{
   hackage::PackageId,
   hash::{
      Digest,
      OutputHash,
   },
};
use serde::Deserialize;

use crate::{
   fetch::{
      dispatch::Producer,
      drv::{
         Place,
         Plan,
         Row,
         attr,
         read_toml,
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
   src:   Digest,
   cabal: Digest,
}

/// locks/hackage.toml -> one file+noindex repository for all cabal packages,
/// <pkg>-<version>.tar.gz with the revised .cabal beside it.
pub struct Hackage;

impl Producer for Hackage {
   fn name(&self) -> &'static str {
      "hackage"
   }

   fn run(&self, store: &mut Store) -> io::Result<()> {
      let locks: Locks = read_toml(attr("locks")?)?;
      let mut plan = Plan::new("hackage-set");
      for (id, entry) in &locks.hackage {
         plan.fetch(
            Row {
               file: format!("{id}.tar.gz"),
               url:  format!("{HACKAGE}/{id}/{id}.tar.gz"),
               hash: OutputHash::sri(entry.src.clone()),
            },
            Place::Link(format!("{id}.tar.gz")),
         );
         plan.fetch(
            Row {
               file: format!("{id}-r{}.cabal", entry.rev),
               url:  format!("{HACKAGE}/{id}/revision/{}.cabal", entry.rev),
               hash: OutputHash::sri(entry.cabal.clone()),
            },
            Place::Link(format!("{id}.cabal")),
         );
      }
      say(&format!("hackageSet: {} packages", locks.hackage.len()));
      plan.submit(store)
   }
}
