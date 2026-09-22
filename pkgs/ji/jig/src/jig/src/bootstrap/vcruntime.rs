use std::{
   fs::create_dir_all,
   io,
   path::Path,
};

use crate::bootstrap::{
   dispatch::Recipe,
   stage::{
      Stage,
      files,
      run,
      strs,
      wanted,
   },
};

pub struct Vcruntime;

impl Recipe for Vcruntime {
   fn name(&self) -> &'static str {
      "vcruntime"
   }

   fn run(&self, stage: &Stage) -> io::Result<()> {
      let sdk = wanted(stage.sdk.as_deref(), "sdk")?;
      let bin = Path::new(stage.out()).join("bin");
      create_dir_all(&bin)?;
      let mut args: Vec<String> = files(&format!("{sdk}/crt/redist/*.dll"))?
         .into_iter()
         .map(|path| path.to_string_lossy().into_owned())
         .collect();
      args.push(bin.to_string_lossy().into_owned());
      run("cp", &strs(&args))
   }
}
