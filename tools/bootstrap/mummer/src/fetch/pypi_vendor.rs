use std::{
   collections::BTreeMap,
   fs::read_to_string,
   io,
   path::Path,
   slice,
};

use mummer::{
   store_path::{
      DrvPath,
      StorePath,
   },
   unpack::Bsdtar,
};
use serde::{
   Deserialize,
   Serialize,
};

use crate::{
   fetch::{
      cargo::vendor_layout,
      dispatch::Producer,
      drv::{
         Assembly,
         Entry,
         NoAttrs,
         attr,
         collect_with,
         say,
         store_attr,
      },
   },
   nix_store::Store,
};

#[derive(Deserialize, Serialize)]
pub struct Sdist {
   pub name: String,
   pub out:  StorePath,
}

#[derive(Deserialize, Serialize)]
pub struct Tree {
   pub name: String,
   pub dir:  String,
}

#[derive(Deserialize, Serialize)]
pub struct Stage {
   pub layout: Vec<Entry>,
   pub drvs:   Vec<DrvPath>,
   pub srcs:   Vec<StorePath>,
   pub sdists: Vec<Sdist>,
   pub trees:  Vec<Tree>,
}

pub struct PypiVendor;

impl Producer for PypiVendor {
   fn name(&self) -> &'static str {
      "pypi-vendor"
   }

   fn run(&self, store: &mut Store) -> io::Result<()> {
      let mut stage = serde_json::from_str::<Stage>(&read_to_string(attr("stage_attrs")?)?)?;
      let tar = Bsdtar::new(store_attr("peanut")?.join("bin/bsdtar"));
      let mut locks = Vec::new();

      for sdist in &stage.sdists {
         let entries = tar.list(sdist.out.as_ref())?;

         if let Some(lock) = entries.lines().find(|entry| {
            let path = Path::new(entry);
            path.components().count() == 2
               && path.file_name().is_some_and(|name| name == "Cargo.lock")
         }) {
            locks.push((sdist.name.as_str(), tar.read(sdist.out.as_ref(), lock)?));
         }
      }

      for tree in &stage.trees {
         let lock = Path::new(&tree.dir).join("Cargo.lock");

         if lock.exists() {
            locks.push((tree.name.as_str(), read_to_string(lock)?));
         }
      }

      for &(name, ref lock) in &locks {
         let vendor = vendor_layout(
            store,
            slice::from_ref(lock),
            &format!("vendor/{name}/"),
            &BTreeMap::new(),
         )?;
         stage.layout.extend(vendor.layout);
         stage.drvs.extend(vendor.drvs);
      }

      if !locks.is_empty() {
         say(&format!(
            "pythonDeps: crates for {}",
            locks
               .iter()
               .map(|entry| entry.0)
               .collect::<Vec<_>>()
               .join(" ")
         ));
      }

      collect_with(store, "python-deps", &stage.layout, &stage.drvs, Assembly {
         script: "assemble",
         attrs:  NoAttrs,
         srcs:   &stage.srcs,
      })
   }
}
