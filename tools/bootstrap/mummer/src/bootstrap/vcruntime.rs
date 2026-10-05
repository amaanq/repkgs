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
      files,
   },
};

#[derive(Deserialize)]
struct Attrs {
   sdk: PathBuf,
}

pub struct Vcruntime;

impl Recipe for Vcruntime {
   fn name(&self) -> &'static str {
      "vcruntime"
   }

   fn run(&self, stage: &Stage) -> io::Result<()> {
      let attrs = load::<Attrs>()?;
      let bin = stage.out.join("bin");
      create_dir_all(&bin)?;
      execute(
         Command::new("cp")
            .args(files(&format!("{}/crt/redist/*.dll", attrs.sdk.display()))?)
            .arg(&bin),
      )
   }
}
