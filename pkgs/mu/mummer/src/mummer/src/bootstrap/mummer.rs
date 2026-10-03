use std::{
   fs::{
      copy,
      create_dir_all,
   },
   io,
   os::unix::fs::symlink,
};

use crate::bootstrap::{
   dispatch::Recipe,
   stage::{
      Stage,
      wanted,
   },
};

pub struct Mummer;

impl Recipe for Mummer {
   fn name(&self) -> &'static str {
      "mummer"
   }

   fn run(&self, stage: &Stage) -> io::Result<()> {
      let prebuilt = wanted(stage.prebuilt.as_deref(), "prebuilt")?;
      let bin = format!("{}/bin", stage.out());
      create_dir_all(&bin)?;
      copy(format!("{prebuilt}/bin/mummer"), format!("{bin}/mummer"))?;
      for name in ["clang", "clang++"] {
         symlink("mummer", format!("{bin}/{name}"))?;
      }
      Ok(())
   }
}
