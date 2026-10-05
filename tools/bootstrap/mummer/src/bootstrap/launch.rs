use std::{
   fs::{
      create_dir_all,
      metadata,
   },
   io,
   path::PathBuf,
   process::Command,
};

use mummer::attrs::load;
use serde::Deserialize;

use crate::bootstrap::{
   dispatch::Recipe,
   stage::{
      Stage,
      execute,
      say,
   },
   target::{
      Cpu,
      Os,
   },
};

#[derive(Deserialize)]
struct Attrs {
   launch: PathBuf,
}

pub struct Launch;

impl Recipe for Launch {
   fn name(&self) -> &'static str {
      "launch"
   }

   /// static linkage leaves no interpreter or RUNPATH of its own to relocate.
   fn run(&self, stage: &Stage) -> io::Result<()> {
      let attrs = load::<Attrs>()?;
      let bin = stage.out.join("bin");
      create_dir_all(&bin)?;
      let linkage = if stage.os == Os::Windows || stage.cpu == Cpu::Powerpc64le {
         "-static"
      } else {
         "-static-pie"
      };
      let exe = bin.join(format!("launch{}", stage.profile.exe()));
      execute(
         Command::new("cc")
            .args([
               "-std=c17", "-O2", "-Wall", "-Wextra", "-Werror", linkage, "-o",
            ])
            .arg(&exe)
            .arg(&attrs.launch),
      )?;
      execute(Command::new("llvm-strip").arg("--strip-all").arg(&exe))?;
      say(&format!("launch: {}", metadata(&exe)?.len()));
      Ok(())
   }
}
