use std::{
   ffi::OsStr,
   fs::{
      OpenOptions,
      create_dir_all,
      read,
      read_dir,
      read_to_string,
      remove_file,
   },
   io::{
      self,
      Write as _,
   },
   os::unix::{
      ffi::OsStrExt as _,
      fs::symlink,
   },
   path::{
      Path,
      PathBuf,
   },
   process::{
      Command,
      Stdio,
   },
};

use mummer::{
   attrs::load,
   files::{
      remove_if_exists,
      write_file,
   },
};
use serde::Deserialize;

use crate::bootstrap::{
   dispatch::Recipe,
   stage::{
      LibcAttrs,
      Stage,
      capture,
      cc_facts,
      configure,
      cores,
      execute,
      files,
      say,
      tool,
   },
   target::Cpu,
};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Attrs {
   #[serde(flatten)]
   libc:          LibcAttrs,
   linux_headers: PathBuf,
   #[serde(default)]
   locale:        bool,
}

pub struct Glibc;

const PREFIX: &str = "/build/glibc";

impl Glibc {
   fn patched_source(stage: &Stage) -> io::Result<PathBuf> {
      let src = stage.unpack("glibc", &[])?;
      if stage.cpu == Cpu::Loongarch64 {
         let found = Command::new("grep")
            .args([
               "-rl",
               r#""=f" (\(x_cond\|fn_cond\|cls\))"#,
               "sysdeps/loongarch",
            ])
            .current_dir(&src)
            .output()?;
         // grep exits 1 when nothing matches, 2 on an error
         if found.status.code().is_none_or(|code| code > 1_i32) {
            return Err(io::Error::other(format!("grep failed, {}", found.status)));
         }
         for line in found.stdout.split(|byte| *byte == b'\n') {
            if !line.is_empty() {
               remove_file(src.join(OsStr::from_bytes(line)))?;
            }
         }
      }
      for barrier in [
         "sysdeps/loongarch/fpu/math-barriers.h",
         "sysdeps/powerpc/fpu/math-barriers.h",
      ] {
         remove_if_exists(&src.join(barrier))?;
      }
      Ok(src)
   }

   const fn cpu_flags(cpu: Cpu) -> &'static [&'static str] {
      match cpu {
         Cpu::X86_64 => {
            &[
               "--enable-cet",
               "libc_cv_have_x86_lahf_sahf=yes",
               "libc_cv_have_x86_movbe=yes",
            ]
         },
         // ldbl-opt's -mlong-double-128 probe is written as a nested function, a GCC extension
         Cpu::Powerpc64le => {
            &[
               "--with-cpu=power9",
               "--enable-fortify-source=no",
               "libc_cv_no_gnu_attr_ok=yes",
               "libc_cv_mlong_double_128=yes",
            ]
         },
         Cpu::Aarch64 | Cpu::Riscv64 | Cpu::Loongarch64 => &[],
      }
   }

   fn print_configure_log(build: &Path) {
      let Ok(log) = read_to_string(build.join("config.log")) else {
         return;
      };
      let filtered = log
         .lines()
         .filter(|line| {
            line.to_ascii_lowercase().contains("error")
               || (line.starts_with("configure:")
                  && (line.contains(": checking") || line.contains(": result")))
               || (line.contains("clang ")
                  && line.starts_with(|character: char| {
                     character == '/' || character.is_ascii_lowercase()
                  }))
         })
         .collect::<Vec<_>>();
      let tail = &filtered[filtered.len().saturating_sub(40)..];
      drop(writeln!(io::stderr().lock(), "{}", tail.join("\n")));
   }

   /// CXX=false, a working c++ would make the build compile a C++ test helper
   /// against our headers.
   fn configure(
      build: &Path,
      src: &Path,
      stage: &Stage,
      linux_headers: &Path,
      rt: &Path,
      sh: &str,
   ) -> io::Result<()> {
      let cc = stage.raw_cc(rt, &[], &[]);
      let ldflags = format!("-L{}/lib/{}", rt.display(), stage.clang_target);
      let result = execute(
         configure(build, &src.join("configure"), PREFIX)
            .arg("--sysconfdir=/etc")
            .args(stage.host_build())
            .arg(format!(
               "--with-headers={}/include",
               linux_headers.display()
            ))
            .args([
               "--enable-kernel=3.10",
               "--disable-werror",
               "--disable-nscd",
               "--enable-bind-now",
               "--enable-fortify-source",
               "--enable-stack-protector=strong",
            ])
            .arg(format!("libc_cv_slibdir={PREFIX}/lib"))
            .arg(format!("libc_cv_rtlddir={PREFIX}/lib"))
            .args(Self::cpu_flags(stage.cpu))
            .envs([
               ("SHELL", sh),
               ("CONFIG_SHELL", sh),
               ("CC", &cc),
               ("CXX", "false"),
               ("BUILD_CC", "cc"),
               ("LDFLAGS", &ldflags),
               ("LD", "ld.lld"),
               ("AR", "llvm-ar"),
               ("NM", "llvm-nm"),
               ("OBJCOPY", "llvm-objcopy"),
               ("OBJDUMP", "llvm-objdump"),
               ("READELF", "llvm-readelf"),
               ("STRIP", "llvm-strip"),
            ]),
      );
      if result.is_err() {
         Self::print_configure_log(build);
      }
      result
   }

   fn install_tree(dest: &Path, out: &Path) -> io::Result<()> {
      create_dir_all(out)?;
      for entry in read_dir(dest.join(PREFIX.trim_start_matches('/')))? {
         execute(Command::new("mv").arg(entry?.path()).arg(out))?;
      }
      for script in files(&format!(
         "{}/bin/{{ldd,sotruss,xtrace,tzselect}}",
         out.display()
      ))? {
         let text = read_to_string(&script)?.replace(PREFIX, "${0%/*}/..");
         write_file(&script, text.as_bytes(), None)?;
      }
      for library in files(&format!("{}/lib/lib{{c,m}}.{{so,a}}", out.display()))? {
         let bytes = read(&library)?;
         if !bytes.starts_with(b"/* GNU ld script") {
            continue;
         }
         let text = String::from_utf8(bytes)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?
            .replace(&format!("{PREFIX}/lib/"), "");
         write_file(&library, text.as_bytes(), None)?;
      }
      let lib = out.join("lib");
      // glibc 2.34 folded these into libc and stopped installing the
      // linker names that -ldl and ctypes find_library still look up
      for (name, soname) in [
         ("libdl.so", "libdl.so.2"),
         ("libpthread.so", "libpthread.so.0"),
         ("librt.so", "librt.so.1"),
         ("libutil.so", "libutil.so.1"),
      ] {
         if lib.join(soname).exists() {
            symlink(soname, lib.join(name))?;
         }
      }
      Ok(())
   }

   fn make_logged(build: &Path, args: &[&str], log_path: &Path) -> io::Result<()> {
      drop(writeln!(
         io::stderr().lock(),
         "+ make {} > make.log",
         args.join(" ")
      ));
      let file = OpenOptions::new()
         .create(true)
         .append(true)
         .open(log_path)?;
      let status = Command::new("make")
         .args(args)
         .current_dir(build)
         .stdout(Stdio::from(file.try_clone()?))
         .stderr(Stdio::from(file))
         .status()?;
      if status.success() {
         return Ok(());
      }
      let log = read_to_string(log_path)?;
      let mut shown = log
         .lines()
         .filter(|line| {
            (line.to_ascii_lowercase().contains("error") || line.contains("***"))
               && !line.contains("Werror")
               && !line.contains("-Wno-error")
         })
         .collect::<Vec<_>>();
      let quiet = log
         .lines()
         .filter(|line| {
            ![
               "reassign symbol",
               "static-libgcc",
               "overriding recipe",
               "ignoring old recipe",
               " -c ",
            ]
            .iter()
            .any(|noise| line.contains(noise))
         })
         .collect::<Vec<_>>();
      shown.extend_from_slice(&quiet[quiet.len().saturating_sub(100)..]);
      drop(writeln!(io::stderr().lock(), "{}", shown.join("\n")));
      let target = args.last().copied().unwrap_or("target");
      Err(io::Error::other(format!(
         "glibc make {target} failed, {status}"
      )))
   }

   /// C.UTF-8 so LC_ALL=C.UTF-8 works everywhere without a locales package,
   /// charmap data only, ~360 K.
   fn c_utf8(stage: &Stage, src: &Path) -> io::Result<()> {
      let lib = stage.out.join("lib");
      create_dir_all(lib.join("locale"))?;
      execute(
         Command::new(lib.join(&stage.interp))
            .arg("--library-path")
            .arg(&lib)
            .arg(stage.out.join("bin/localedef"))
            .args(["--no-archive", "-i", "C", "-f", "UTF-8"])
            .arg(lib.join("locale/C.utf8"))
            .env("I18NPATH", src.join("localedata")),
      )
   }
}

impl Recipe for Glibc {
   fn name(&self) -> &'static str {
      "glibc"
   }

   fn run(&self, stage: &Stage) -> io::Result<()> {
      let attrs = load::<Attrs>()?;
      let out = &stage.out;
      let src = Self::patched_source(stage)?;
      let build = stage.build_top.join("build");
      create_dir_all(&build)?;
      write_file(&build.join("configparms"), b"with-clang = yes\n", None)?;

      let rt = match attrs.libc.compiler_rt() {
         Some(compiler_rt) => compiler_rt.to_path_buf(),
         None => {
            PathBuf::from(
               capture(Command::new(stage.llvm.join("bin/clang")).arg("--print-resource-dir"))?
                  .trim(),
            )
         },
      };
      let sh = tool("sh")?.display().to_string();
      Self::configure(&build, &src, stage, &attrs.linux_headers, &rt, &sh)?;

      // Makeconfig's lazy $(shell $(CC) -print-file-name=libgcc_s.so.1) expands
      // ~600 times per build. There is no libgcc_s here with compiler-rt,
      // so the answer is empty.
      let jobs = cores().to_string();
      let shell = format!("SHELL={sh}");
      let sysincludes = format!(
         "sysincludes=-nostdinc -isystem {}/include -isystem {}/include",
         rt.display(),
         attrs.linux_headers.display()
      );
      let make_base = [
         "-j",
         &jobs,
         &shell,
         &sysincludes,
         "gnulib-extralibdir=",
         "zonedir=/usr/share/zoneinfo",
      ];
      let log_path = stage.build_top.join("make.log");
      let dest = stage.build_top.join("dest");
      let destdir = format!("DESTDIR={}", dest.display());

      if attrs.libc.headers_only() {
         Self::make_logged(
            &build,
            &[&make_base[..], &["install-headers", &destdir]].concat(),
            &log_path,
         )?;
         Self::install_tree(&dest, out)?;
         let stubs = out.join("include/gnu/stubs.h");
         if !stubs.exists() {
            write_file(&stubs, b"", None)?;
         }
         return cc_facts(out, &[("include-dirs", &["include"])]);
      }

      Self::make_logged(&build, &make_base, &log_path)?;
      // Reading depfiles converts each .dt to .d in place, which parallel
      // sub-makes race on. Nothing is stale right after the build, so the
      // install needs no dependency information.
      Self::make_logged(
         &build,
         &[&make_base[..], &["install", "no_deps=t", &destdir]].concat(),
         &log_path,
      )?;
      Self::install_tree(&dest, out)?;
      if attrs.locale {
         Self::c_utf8(stage, &src)?;
      }
      let lib_count = read_dir(out.join("lib"))?
         .collect::<io::Result<Vec<_>>>()?
         .len();
      cc_facts(out, &[("include-dirs", &["include"])])?;
      say(&format!("glibc {}: {lib_count} files in lib/", stage.cpu));
      Ok(())
   }
}
