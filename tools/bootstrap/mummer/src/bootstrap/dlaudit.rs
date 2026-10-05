use std::{
   fs::create_dir_all,
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
   },
};

#[derive(Deserialize)]
struct Attrs {
   dlaudit: PathBuf,
}

pub struct Dlaudit;

impl Recipe for Dlaudit {
   fn name(&self) -> &'static str {
      "dlaudit"
   }

   fn run(&self, stage: &Stage) -> io::Result<()> {
      let attrs = load::<Attrs>()?;
      let lib = stage.out.join("lib");
      create_dir_all(&lib)?;
      execute(
         Command::new("cc")
            .args([
               "-std=c17", "-O2", "-Wall", "-Wextra", "-Werror", "-shared", "-fPIC", "-o",
            ])
            .arg(lib.join("dlaudit.so"))
            .arg(&attrs.dlaudit),
      )
   }
}
