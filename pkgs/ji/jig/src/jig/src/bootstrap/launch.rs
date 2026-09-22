use std::{
   fs::{
      copy,
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
      let json_hpp = wanted(stage.json_hpp.as_deref(), "json_hpp")?;
      let json = format!("{}/json", stage.build_top);
      let bin = format!("{}/bin", stage.out());
      create_dir_all(&json)?;
      create_dir_all(&bin)?;
      copy(json_hpp, format!("{json}/json.hpp"))?;
      let windows = stage.os == "windows";
      let linkage = if windows || stage.cpu == "powerpc64le" {
         "-static"
      } else {
         "-static-pie"
      };
      let suffix = if windows { ".exe" } else { "" };
      let exe = format!("{bin}/launch{suffix}");
      run("c++", &[
         "-std=c++23",
         "-O2",
         "-Wall",
         "-Wextra",
         "-Werror",
         "-fno-exceptions",
         "-fno-rtti",
         linkage,
         "-isystem",
         &json,
         "-o",
         &exe,
         source,
      ])?;
      run("llvm-strip", &["--strip-all", &exe])?;
      say(&format!("launch: {}", metadata(&exe)?.len()));
      Ok(())
   }
}
