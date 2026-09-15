use std::{
   fs::{
      create_dir_all,
      read_dir,
   },
   io,
   path::Path,
};

use crate::bootstrap::{
   dispatch::Recipe,
   stage::{
      Stage,
      cc_facts,
      cores,
      run,
      run_in_env,
      say,
      tool,
      wanted,
   },
};

const BINUTILS: [(&str, &str); 8] = [
   ("AR", "llvm-ar"),
   ("RANLIB", "llvm-ranlib"),
   ("DLLTOOL", "llvm-dlltool"),
   ("NM", "llvm-nm"),
   ("OBJCOPY", "llvm-objcopy"),
   ("STRIP", "llvm-strip"),
   ("RC", "llvm-windres"),
   ("WINDRES", "llvm-windres"),
];

pub struct Mingw;

impl Mingw {
   fn configure_make(
      stage: &Stage,
      src: &Path,
      sub: &str,
      args: &[&str],
      vars: &[(&str, &str)],
   ) -> io::Result<()> {
      let build = Path::new(&stage.build_top).join(format!("build-{}", sub.replace('/', "-")));
      create_dir_all(&build)?;
      let sh = tool("sh")?.to_string_lossy().into_owned();
      let mut env = Vec::from(BINUTILS);
      env.push(("CONFIG_SHELL", &sh));
      env.extend_from_slice(vars);
      let configure_script = src
         .join(sub)
         .join("configure")
         .to_string_lossy()
         .into_owned();
      let prefix_arg = format!("--prefix={}", stage.out());
      let host_arg = format!("--host={}", stage.clang_target);
      let build_arg = format!("--build={}", stage.build_triple);
      let mut configure = vec![
         configure_script.as_str(),
         &prefix_arg,
         &host_arg,
         &build_arg,
         "--disable-dependency-tracking",
      ];
      configure.extend_from_slice(args);
      run_in_env(Some(&build), "sh", &configure, &env)?;
      let jobs = cores().to_string();
      let shell_arg = format!("SHELL={sh}");
      run_in_env(Some(&build), "make", &["-j", &jobs, &shell_arg], &env)?;
      run_in_env(Some(&build), "make", &["install", &shell_arg], &env)
   }
}

impl Recipe for Mingw {
   fn name(&self) -> &'static str {
      "mingw-w64"
   }

   fn run(&self, stage: &Stage) -> io::Result<()> {
      let out = stage.out();
      let src = stage.unpack("mingw-w64", &[])?;
      Self::configure_make(
         stage,
         &src,
         "mingw-w64-headers",
         &[
            "--enable-idl",
            "--with-default-msvcrt=ucrt",
            "--with-default-win32-winnt=0x0A00",
         ],
         &[],
      )?;
      cc_facts(out, &[("include-dirs", &["include"])])?;
      if stage.headers_only() {
         return Ok(());
      }

      let rt = wanted(stage.compiler_rt.as_deref(), "compiler-rt")?;
      let cc = format!(
         "clang {} -resource-dir={rt} -isystem {out}/include --start-no-unused-arguments \
          -rtlib=compiler-rt -unwindlib=none -fuse-ld=lld -L{out}/lib --end-no-unused-arguments",
         stage.target().join(" "),
      );
      let mut crt_args = if stage.cpu == "aarch64" {
         vec!["--disable-lib32", "--disable-lib64", "--enable-libarm64"]
      } else {
         vec!["--disable-lib32", "--enable-lib64"]
      };
      crt_args.extend(["--with-default-msvcrt=ucrt", "--enable-cfguard"]);
      let includes = format!("-I{out}/include");
      Self::configure_make(stage, &src, "mingw-w64-crt", &crt_args, &[
         ("CC", &cc),
         ("CCAS", &cc),
         ("CPPFLAGS", &includes),
      ])?;
      for lib in ["ssp", "ssp_nonshared"] {
         run("llvm-ar", &["rcs", &format!("{out}/lib/lib{lib}.a")])?;
      }
      let ldflags = format!("-L{out}/lib");
      Self::configure_make(
         stage,
         &src,
         "mingw-w64-libraries/winpthreads",
         &["--enable-static", "--enable-shared"],
         &[
            ("CC", &cc),
            ("CPPFLAGS", &includes),
            ("RCFLAGS", &includes),
            ("LDFLAGS", &ldflags),
         ],
      )?;
      let count = read_dir(format!("{out}/lib"))?
         .try_fold(0_usize, |count, entry| entry.map(|_entry| count + 1))?;
      say(&format!("mingw-w64: {count} files in lib/"));
      Ok(())
   }
}
