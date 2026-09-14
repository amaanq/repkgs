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

   /// header-only C++ and -nostdlib++, so nothing but libc is in the audited
   /// process.
   fn run(&self, stage: &Stage) -> io::Result<()> {
      let source = wanted(stage.dlaudit.as_deref(), "dlaudit")?;
      let lib = format!("{}/lib", stage.out());
      create_dir_all(&lib)?;
      run("c++", &[
         "-std=c++23",
         "-O2",
         "-Wall",
         "-Wextra",
         "-Werror",
         "-fno-exceptions",
         "-fno-rtti",
         "-shared",
         "-fPIC",
         "-nostdlib++",
         "-o",
         &format!("{lib}/dlaudit.so"),
         source,
      ])
   }
}
