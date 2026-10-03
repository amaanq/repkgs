use std::{
   io,
   path::Path,
};

use shim::store::on_path;

use crate::bootstrap::{
   dispatch::Recipe,
   stage::{
      Stage,
      copy_tree,
      cores,
      files,
      run_in,
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
   /// build machine, stage0's cc in stage1 and the crib clang in stage0.
   fn run(&self, stage: &Stage) -> io::Result<()> {
      let out = stage.out();
      let src = stage.unpack("linux", &[])?;
      let hostcc = if on_path("cc").is_absolute() {
         "cc".to_owned()
      } else {
         let mut words = vec!["clang".to_owned()];
         words.extend(stage.ccflags()?);
         words.push("-static-pie".to_owned());
         words.join(" ")
      };
      let sh = tool("sh")?.to_string_lossy().into_owned();
      run_in(&src, "make", &[
         "-j",
         &cores().to_string(),
         "headers",
         &format!("ARCH={}", stage.karch),
         &format!("HOSTCC={hostcc}"),
         &format!("SHELL={sh}"),
         &format!("CONFIG_SHELL={sh}"),
      ])?;
      let include = format!("{out}/include");
      copy_tree(&src.join("usr/include"), Path::new(&include), "**/*.h")?;
      say(&format!(
         "linux-headers: {} headers",
         files(&format!("{include}/**/*.h"))?.len()
      ));
      Ok(())
   }
}
