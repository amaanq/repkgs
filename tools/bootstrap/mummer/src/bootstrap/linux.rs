use std::{
   io,
   process::Command,
};

use mummer::store::on_path;

use crate::bootstrap::{
   dispatch::Recipe,
   stage::{
      Stage,
      copy_tree,
      cores,
      execute,
      files,
      say,
      tool,
   },
};

pub struct Linux;

impl Recipe for Linux {
   fn name(&self) -> &'static str {
      "linux-headers"
   }

   /// the kernel's own `make headers`, HOSTCC builds unifdef and co. for the
   /// build machine, stage0's cc in stage1 and the peanut clang in stage0.
   fn run(&self, stage: &Stage) -> io::Result<()> {
      let src = stage.unpack("linux", &[])?;
      let hostcc = if on_path("cc").is_absolute() {
         "cc".to_owned()
      } else {
         let mut words = vec!["clang".to_owned()];
         words.extend(stage.ccflags()?);
         words.push("-static-pie".to_owned());
         words.join(" ")
      };
      let sh = tool("sh")?.display().to_string();
      execute(
         Command::new("make")
            .arg("-j")
            .arg(cores().to_string())
            .arg("headers")
            .arg(format!("ARCH={}", stage.karch))
            .arg(format!("HOSTCC={hostcc}"))
            .arg(format!("SHELL={sh}"))
            .arg(format!("CONFIG_SHELL={sh}"))
            .current_dir(&src),
      )?;
      let include = stage.out.join("include");
      copy_tree(&src.join("usr/include"), &include, "**/*.h")?;
      say(&format!(
         "linux-headers: {} headers",
         files(&format!("{}/**/*.h", include.display()))?.len()
      ));
      Ok(())
   }
}
