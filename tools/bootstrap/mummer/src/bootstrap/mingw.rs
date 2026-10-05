use std::{
   fs::{
      create_dir_all,
      read_dir,
   },
   io,
   path::Path,
   process::Command,
};

use mummer::attrs::load;

use crate::bootstrap::{
   dispatch::Recipe,
   stage::{
      LibcAttrs,
      Stage,
      cc_facts,
      configure,
      execute,
      make_parallel,
      make_target,
      say,
      tool,
   },
   target::{
      Cpu,
      Profile,
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
      let build = stage
         .build_top
         .join(format!("build-{}", sub.replace('/', "-")));
      create_dir_all(&build)?;
      let shell = tool("sh")?.display().to_string();
      let environment = || {
         BINUTILS
            .iter()
            .copied()
            .chain([("CONFIG_SHELL", shell.as_str())])
            .chain(vars.iter().copied())
      };
      execute(
         configure(
            &build,
            &src.join(sub).join("configure"),
            stage.out.display(),
         )
         .args(stage.host_build())
         .arg("--disable-dependency-tracking")
         .args(args)
         .envs(environment()),
      )?;
      execute(make_parallel(&build)?.envs(environment()))?;
      execute(make_target(&build, "install")?.envs(environment()))
   }
}

impl Recipe for Mingw {
   fn name(&self) -> &'static str {
      "mingw-w64"
   }

   fn run(&self, stage: &Stage) -> io::Result<()> {
      let attrs = load::<LibcAttrs>()?;
      let out = &stage.out;
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
      if attrs.headers_only() {
         return Ok(());
      }

      let cc = stage.raw_cc(
         attrs.required_compiler_rt()?,
         &[
            "-isystem".to_owned(),
            out.join("include").display().to_string(),
         ],
         &[format!("-L{}/lib", out.display())],
      );
      let mut crt_args = if stage.cpu == Cpu::Aarch64 {
         vec!["--disable-lib32", "--disable-lib64", "--enable-libarm64"]
      } else {
         vec!["--disable-lib32", "--enable-lib64"]
      };
      crt_args.extend(["--with-default-msvcrt=ucrt", "--enable-cfguard"]);
      let includes = format!("-I{}/include", out.display());
      Self::configure_make(stage, &src, "mingw-w64-crt", &crt_args, &[
         ("CC", &cc),
         ("CCAS", &cc),
         ("CPPFLAGS", &includes),
      ])?;
      for lib in ["ssp", "ssp_nonshared"] {
         execute(
            Command::new("llvm-ar")
               .arg("rcs")
               .arg(out.join(format!("lib/lib{lib}.a"))),
         )?;
      }
      let Profile::Mingw(machine) = stage.profile else {
         return Err(io::Error::other("combase requires a mingw target"));
      };
      execute(
         Command::new("llvm-dlltool")
            .args(["-m", machine.dlltool(), "-d"])
            .arg(src.join("mingw-w64-crt/libarm32/combase.def"))
            .arg("-l")
            .arg(out.join("lib/libcombase.a")),
      )?;

      let ldflags = format!("-L{}/lib", out.display());
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
      let count = read_dir(out.join("lib"))?
         .try_fold(0_usize, |count, entry| entry.map(|_entry| count + 1))?;
      say(&format!("mingw-w64: {count} files in lib/"));
      Ok(())
   }
}
