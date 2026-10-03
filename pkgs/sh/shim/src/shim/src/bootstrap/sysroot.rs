use std::{
   fs::{
      canonicalize,
      copy,
      create_dir_all,
      remove_file,
   },
   io,
   os::unix::fs::symlink,
   path::Path,
};

use shim::files::write_file;

use crate::bootstrap::{
   dispatch::Recipe,
   stage::{
      Stage,
      files,
      run,
      wanted,
   },
};

pub struct Sysroot;

impl Recipe for Sysroot {
   fn name(&self) -> &'static str {
      "sysroot"
   }

   /// the parts merged as symlink trees, later parts win, musl and the kernel
   /// both ship include/scsi. MacOSX.sdk brings a real usr/, ELF sysroots
   /// alias it.
   fn run(&self, stage: &Stage) -> io::Result<()> {
      let out = stage.out();
      let resource = wanted(stage.resource.as_deref(), "resource")?;
      create_dir_all(format!("{out}/include"))?;
      create_dir_all(format!("{out}/lib"))?;
      for part in &stage.parts {
         run("cp", &["-rsf", &format!("{part}/."), &format!("{out}/")])?;
      }
      run("cp", &["-rs", resource, &format!("{out}/lib/clang")])?;
      if !Path::new(&format!("{out}/usr")).exists() {
         symlink(".", format!("{out}/usr"))?;
      }
      symlink("lib", format!("{out}/lib64"))?;
      for object in files(&format!("{out}/lib/*crt*.o"))? {
         if object.is_symlink() {
            let target = canonicalize(&object)?;
            remove_file(&object)?;
            copy(target, &object)?;
         }
      }
      write_file(
         Path::new(&format!("{out}/roots")),
         format!("{} {resource}\n", stage.parts.join(" ")).as_bytes(),
         None,
      )
   }
}
