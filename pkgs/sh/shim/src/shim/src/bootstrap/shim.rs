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

pub struct Shim;

impl Recipe for Shim {
   fn name(&self) -> &'static str {
      "shim"
   }

   fn run(&self, stage: &Stage) -> io::Result<()> {
      let prebuilt = wanted(stage.prebuilt.as_deref(), "prebuilt")?;
      let bin = format!("{}/bin", stage.out());
      create_dir_all(&bin)?;
      copy(format!("{prebuilt}/bin/shim"), format!("{bin}/shim"))?;
      for name in ["clang", "clang++"] {
         symlink("shim", format!("{bin}/{name}"))?;
      }
      Ok(())
   }
}
