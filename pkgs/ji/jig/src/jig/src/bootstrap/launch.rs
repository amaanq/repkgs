use std::{
   fs::{
      create_dir_all,
      metadata,
   },
   io,
};

use crate::bootstrap::{
   dispatch::Recipe,
   stage::{
      Stage,
      run,
      say,
      wanted,
   },
};

pub struct Launch;

impl Recipe for Launch {
   fn name(&self) -> &'static str {
      "launch"
   }

   /// static linkage leaves no interpreter or RUNPATH of its own to relocate.
   fn run(&self, stage: &Stage) -> io::Result<()> {
      let source = wanted(stage.launch.as_deref(), "launch")?;
      let bin = format!("{}/bin", stage.out());
      create_dir_all(&bin)?;
      let windows = stage.os == "windows";
      let linkage = if windows || stage.cpu == "powerpc64le" {
         "-static"
      } else {
         "-static-pie"
      };
      let suffix = if windows { ".exe" } else { "" };
      let exe = format!("{bin}/launch{suffix}");
      run("cc", &[
         "-std=c17", "-O2", "-Wall", "-Wextra", "-Werror", linkage, "-o", &exe, source,
      ])?;
      run("llvm-strip", &["--strip-all", &exe])?;
      say(&format!("launch: {}", metadata(&exe)?.len()));
      Ok(())
   }
}
