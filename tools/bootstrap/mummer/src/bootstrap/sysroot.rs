use std::{
   fs::{
      canonicalize,
      copy,
      create_dir_all,
      remove_file,
   },
   io,
   os::unix::fs::symlink,
   path::PathBuf,
   process::Command,
};

use mummer::{
   attrs::load,
   files::{
      utf8,
      write_file,
   },
};
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
   resource: PathBuf,
   parts:    Vec<PathBuf>,
}

pub struct Sysroot;

impl Recipe for Sysroot {
   fn name(&self) -> &'static str {
      "sysroot"
   }

   /// the parts merged as symlink trees, later parts win, musl and the kernel
   /// both ship include/scsi. MacOSX.sdk brings a real usr/, ELF sysroots
   /// alias it.
   fn run(&self, stage: &Stage) -> io::Result<()> {
      let attrs = load::<Attrs>()?;
      let out = &stage.out;
      create_dir_all(out.join("include"))?;
      create_dir_all(out.join("lib"))?;
      for part in &attrs.parts {
         execute(
            Command::new("cp")
               .arg("-rsf")
               .arg(format!("{}/.", part.display()))
               .arg(format!("{}/", out.display())),
         )?;
      }
      execute(
         Command::new("cp")
            .arg("-rs")
            .arg(&attrs.resource)
            .arg(out.join("lib/clang")),
      )?;
      if !out.join("usr").exists() {
         symlink(".", out.join("usr"))?;
      }
      symlink("lib", out.join("lib64"))?;
      for object in files(&format!("{}/lib/*crt*.o", out.display()))? {
         if object.is_symlink() {
            let target = canonicalize(&object)?;
            remove_file(&object)?;
            copy(target, &object)?;
         }
      }
      let parts = attrs
         .parts
         .iter()
         .map(|part| utf8(part))
         .collect::<io::Result<Vec<_>>>()?;
      write_file(
         &out.join("roots"),
         format!("{} {}\n", parts.join(" "), attrs.resource.display()).as_bytes(),
         None,
      )
   }
}
