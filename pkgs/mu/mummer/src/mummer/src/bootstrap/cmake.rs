use std::{
   fs::{
      create_dir_all,
      remove_dir_all,
   },
   io,
   path::PathBuf,
};

use crate::bootstrap::{
   dispatch::Recipe,
   stage::{
      Stage,
      cores,
      dirs,
      run_in,
      run_in_env,
      tool,
      wanted,
   },
};

pub struct Cmake;

impl Recipe for Cmake {
   fn name(&self) -> &'static str {
      "cmake"
   }

   fn run(&self, stage: &Stage) -> io::Result<()> {
      let source = stage.unpack("cmake", &[])?;
      let build = PathBuf::from(&stage.build_top).join("build");
      create_dir_all(&build)?;
      let shell = tool("sh")?.to_string_lossy().into_owned();
      let make = tool("make")?.to_string_lossy().into_owned();
      let jobs = cores().to_string();
      let sysroot = wanted(stage.sysroot.as_deref(), "sysroot")?;
      run_in_env(
         Some(&build),
         &shell,
         &[
            &source.join("bootstrap").to_string_lossy(),
            &format!("--prefix={}", stage.out()),
            &format!("--parallel={jobs}"),
            "--no-system-libs",
            "--no-qt-gui",
            "--",
            "-DCMAKE_USE_OPENSSL=OFF",
            "-DBUILD_TESTING=OFF",
            "-DBUILD_CursesDialog=OFF",
            "-DCMake_BUILD_LTO=OFF",
            &format!("-DCMAKE_SYSTEM_PREFIX_PATH={sysroot}"),
         ],
         &[
            ("CC", "cc"),
            ("CXX", "c++"),
            ("MAKE", &make),
            ("CONFIG_SHELL", &shell),
         ],
      )?;

      let shell_arg = format!("SHELL={shell}");
      run_in(&build, "make", &["-j", &jobs, &shell_arg])?;
      run_in(&build, "make", &["install/fast", &shell_arg])?;

      for path in dirs(&format!("{}/{{doc,share/cmake-*/Help}}", stage.out()))? {
         remove_dir_all(path)?;
      }

      Ok(())
   }
}
