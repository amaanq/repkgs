use std::{
   fs::{
      create_dir_all,
      remove_dir_all,
   },
   io,
   process::Command,
};

use crate::bootstrap::{
   dispatch::Recipe,
   stage::{
      Stage,
      cores,
      dirs,
      execute,
      make_parallel,
      make_target,
      tool,
   },
};

pub struct Cmake;

impl Recipe for Cmake {
   fn name(&self) -> &'static str {
      "cmake"
   }

   fn run(&self, stage: &Stage) -> io::Result<()> {
      let source = stage.unpack("cmake", &[])?;
      let build = stage.build_top.join("build");
      create_dir_all(&build)?;
      let shell = tool("sh")?;
      execute(
         Command::new(&shell)
            .arg(source.join("bootstrap"))
            .arg(format!("--prefix={}", stage.out.display()))
            .arg(format!("--parallel={}", cores()))
            .args([
               "--no-system-libs",
               "--no-qt-gui",
               "--",
               "-DCMAKE_USE_OPENSSL=OFF",
               "-DBUILD_TESTING=OFF",
               "-DBUILD_CursesDialog=OFF",
               "-DCMake_BUILD_LTO=OFF",
            ])
            .arg(format!(
               "-DCMAKE_SYSTEM_PREFIX_PATH={}",
               stage.sysroot()?.display()
            ))
            .env("CC", "cc")
            .env("CXX", "c++")
            .env("MAKE", tool("make")?)
            .env("CONFIG_SHELL", &shell)
            .current_dir(&build),
      )?;
      execute(&mut make_parallel(&build)?)?;
      execute(&mut make_target(&build, "install/fast")?)?;
      for path in dirs(&format!(
         "{}/{{doc,share/cmake-*/Help}}",
         stage.out.display()
      ))? {
         remove_dir_all(path)?;
      }
      Ok(())
   }
}
