use std::{
   env::var,
   fs::{
      create_dir_all,
      read,
      remove_file,
   },
   io,
   os::unix::fs::symlink,
   path::Path,
};

use jig::{
   files::write_file,
   store::STORE_DIR,
};

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

impl Sysroot {
   /// glibc installs libc.so and libm.{so,a} as linker scripts naming
   /// absolute paths, which defeats --sysroot and relocation. Bare names are
   /// looked up in -L dirs.
   fn relativise_ld_scripts(libdir: &str) -> io::Result<()> {
      let store = var("NIX_STORE").unwrap_or_else(|_unset| STORE_DIR.to_owned());
      for file in files(&format!("{libdir}/lib{{c,m}}.{{so,a}}"))? {
         let bytes = read(&file)?;
         if !bytes.starts_with(b"/* GNU ld script") {
            continue;
         }
         let text = String::from_utf8_lossy(&bytes).into_owned();
         let mut stripped = String::new();
         let mut rest = text.as_str();
         let prefix = format!("{store}/");
         while let Some(at) = rest.find(&prefix) {
            stripped.push_str(&rest[..at]);
            let after = &rest[at + prefix.len()..];
            let end = after.find([' ', ')']).unwrap_or(after.len());
            let slash = after[..end].rfind('/').map_or(end, |found| found + 1);
            rest = &after[slash..];
         }
         stripped.push_str(rest);
         remove_file(&file)?;
         write_file(&file, stripped.as_bytes(), None)?;
      }
      Ok(())
   }
}

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
      Self::relativise_ld_scripts(&format!("{out}/lib"))?;
      write_file(
         Path::new(&format!("{out}/roots")),
         format!("{} {resource}\n", stage.parts.join(" ")).as_bytes(),
         None,
      )
   }
}
