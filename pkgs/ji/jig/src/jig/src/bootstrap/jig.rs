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

pub struct Jig;

impl Recipe for Jig {
   fn name(&self) -> &'static str {
      "jig"
   }

   fn run(&self, stage: &Stage) -> io::Result<()> {
      let prebuilt = wanted(stage.prebuilt.as_deref(), "prebuilt")?;
      let bin = format!("{}/bin", stage.out());
      create_dir_all(&bin)?;
      copy(format!("{prebuilt}/bin/jig"), format!("{bin}/jig"))?;
      for name in ["clang", "clang++"] {
         symlink("jig", format!("{bin}/{name}"))?;
      }
      Ok(())
   }
}
