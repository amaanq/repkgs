use std::{
   fs::{
      copy,
      create_dir_all,
   },
   io,
   os::unix::fs::symlink,
   path::PathBuf,
};

use mummer::attrs::load;
use serde::Deserialize;

use crate::bootstrap::{
   dispatch::Recipe,
   stage::Stage,
};

#[derive(Deserialize)]
struct Attrs {
   prebuilt: PathBuf,
}

pub struct Mummer;

impl Recipe for Mummer {
   fn name(&self) -> &'static str {
      "mummer"
   }

   fn run(&self, stage: &Stage) -> io::Result<()> {
      let attrs = load::<Attrs>()?;
      let bin = stage.out.join("bin");
      create_dir_all(&bin)?;
      copy(attrs.prebuilt.join("bin/mummer"), bin.join("mummer"))?;
      for name in ["clang", "clang++"] {
         symlink("mummer", bin.join(name))?;
      }
      Ok(())
   }
}
