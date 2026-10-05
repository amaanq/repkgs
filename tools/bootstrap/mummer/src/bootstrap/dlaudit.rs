use std::{
   fs::create_dir_all,
   io,
};

use crate::bootstrap::{
   dispatch::Recipe,
   stage::{
      Stage,
      run,
      wanted,
   },
};

pub struct Dlaudit;

impl Recipe for Dlaudit {
   fn name(&self) -> &'static str {
      "dlaudit"
   }

   fn run(&self, stage: &Stage) -> io::Result<()> {
      let source = wanted(stage.dlaudit.as_deref(), "dlaudit")?;
      let lib = format!("{}/lib", stage.out());
      create_dir_all(&lib)?;
      run("cc", &[
         "-std=c17",
         "-O2",
         "-Wall",
         "-Wextra",
         "-Werror",
         "-shared",
         "-fPIC",
         "-o",
         &format!("{lib}/dlaudit.so"),
         source,
      ])
   }
}
