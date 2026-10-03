use std::{
   fs::read_to_string,
   io,
   path::Path,
   process::{
      Command,
      Stdio,
   },
};

use serde::{
   Deserialize,
   Serialize,
};
use serde_json::Map;

use crate::{
   fetch::{
      cargo::vendor_layout,
      dispatch::Producer,
      drv::{
         Entry,
         attr,
         collect,
         say,
      },
   },
   nix_store::Store,
};

#[derive(Deserialize, Serialize)]
pub struct Sdist {
   pub name: String,
   pub out:  String,
}

#[derive(Deserialize, Serialize)]
pub struct Tree {
   pub name: String,
   pub dir:  String,
}

#[derive(Deserialize, Serialize)]
pub struct Stage {
   pub layout: Vec<Entry>,
   pub drvs:   Vec<String>,
   pub srcs:   Vec<String>,
   pub sdists: Vec<Sdist>,
   pub trees:  Vec<Tree>,
}

pub struct PypiVendor;

impl Producer for PypiVendor {
   fn name(&self) -> &'static str {
      "pypi-vendor"
   }

   fn run(&self, store: &mut Store) -> io::Result<()> {
      let mut stage: Stage = serde_json::from_str(&read_to_string(attr("stage_attrs")?)?)?;
      let bsdtar = format!("{}/bin/bsdtar", attr("seed")?);
      let archive = |args: &[&str]| -> io::Result<String> {
         let output = Command::new(&bsdtar)
            .args(args)
            .stderr(Stdio::inherit())
            .output()?;

         if !output.status.success() {
            return Err(io::Error::other(format!(
               "bsdtar failed with {}",
               output.status
            )));
         }

         String::from_utf8(output.stdout).map_err(io::Error::other)
      };
      let mut locks = Vec::new();

      for sdist in &stage.sdists {
         let entries = archive(&["-tf", &sdist.out])?;

         if let Some(lock) = entries.lines().find(|entry| {
            let path = Path::new(entry);
            path.components().count() == 2
               && path.file_name().is_some_and(|name| name == "Cargo.lock")
         }) {
            locks.push((sdist.name.as_str(), archive(&["-xOf", &sdist.out, lock])?));
         }
      }

      for tree in &stage.trees {
         let lock = Path::new(&tree.dir).join("Cargo.lock");

         if lock.exists() {
            locks.push((tree.name.as_str(), read_to_string(lock)?));
         }
      }

      for &(name, ref lock) in &locks {
         let vendor = vendor_layout(store, lock, &format!("vendor/{name}/"))?;
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

      collect(
         store,
         "python-deps",
         &stage.layout,
         &stage.drvs,
         None,
         Map::new(),
         &stage.srcs,
      )
   }
}
