use std::io;

use crate::{
   fetch::{
      bun::Bun,
      cargo::Cargo,
      deno::{
         Deno,
         DenoFiles,
      },
      gems::Gems,
      go::Go,
      hackage::Hackage,
      hex::Hex,
      luarocks::Luarocks,
      npm::Npm,
      pnpm::Pnpm,
      pypi::Pypi,
      pypi_vendor::PypiVendor,
      winsdk::Winsdk,
      yarn::Yarn,
   },
   nix_store::Store,
};

/// a lock-file producer as `builder/fetch/<name>.nu` was, run under
/// builder-rpc-v0 with its attrs as environment, it writes derivations through
/// the store and submits the collecting one as its output.
pub trait Producer: Sync {
   fn name(&self) -> &'static str;
   fn run(&self, store: &mut Store) -> io::Result<()>;
}

const PRODUCERS: &[&dyn Producer] = &[
   &Cargo,
   &Npm,
   &Pnpm,
   &Yarn,
   &Bun,
   &Deno,
   &DenoFiles,
   &Pypi,
   &PypiVendor,
   &Gems,
   &Hex,
   &Go,
   &Hackage,
   &Luarocks,
   &Winsdk,
];

pub fn mode(name: &str) -> io::Result<()> {
   let producer = PRODUCERS
      .iter()
      .copied()
      .find(|candidate| candidate.name() == name)
      .ok_or_else(|| io::Error::other(format!("no fetch producer {name}")))?;
   let mut store = Store::open()?;
   producer.run(&mut store)
}
