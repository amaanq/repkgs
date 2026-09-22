use std::{
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
   path::{
      Path,
      PathBuf,
   },
   process::{
      Command,
      Stdio,
   },
};

use jig::files::write_file;

use crate::bootstrap::{
   dispatch::Recipe,
   stage::{
      Stage,
      capture,
      cc_facts,
      cores,
      files,
      run,
      run_in,
      run_in_env,
      run_with,
      say,
      tool,
      wanted,
   },
};

pub struct Glibc;

const PREFIX: &str = "/build/glibc";

impl Glibc {
   fn patched_source(stage: &Stage) -> io::Result<PathBuf> {
      let src = stage.unpack("glibc", &[])?;

      for patch in &stage.patches {
         run_in(&src, "patch", &["-p1", "-i", patch])?;
      }

      if stage.cpu == "loongarch64" {
         let grep_output = Command::new("grep")
            .args([
               "-rl",
               r#""=f" (\(x_cond\|fn_cond\|cls\))"#,
               "sysdeps/loongarch",
            ])
            .current_dir(&src)
            .output()?;

         for line in String::from_utf8_lossy(&grep_output.stdout).lines() {
            if !line.is_empty() {
               remove_file(src.join(line))?;
            }
         }
      }

      for barrier in [
         "sysdeps/loongarch/fpu/math-barriers.h",
         "sysdeps/powerpc/fpu/math-barriers.h",
      ] {
         match remove_file(src.join(barrier)) {
            Ok(()) => {},
            Err(error) if error.kind() == io::ErrorKind::NotFound => {},
            Err(error) => return Err(error),
         }
      }

      Ok(src)
   }

   fn cpu_flags(cpu: &str) -> &'static [&'static str] {
      match cpu {
         "x86_64" => {
            &[
               "--enable-cet",
               "libc_cv_have_x86_lahf_sahf=yes",
               "libc_cv_have_x86_movbe=yes",
            ]
         },
         // ldbl-opt's -mlong-double-128 probe is written as a nested function, a GCC extension
         "powerpc64le" => {
            &[
               "--with-cpu=power9",
               "--enable-fortify-source=no",
               "libc_cv_no_gnu_attr_ok=yes",
               "libc_cv_mlong_double_128=yes",
            ]
         },
         _ => &[],
      }
   }

   fn print_configure_log(build: &Path) {
      let Ok(log) = read_to_string(build.join("config.log")) else {
         return;
      };
      let filtered: Vec<&str> = log
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
         .collect();
      let start = filtered.len().saturating_sub(40);
      let tail = &filtered[start..];
      drop(writeln!(io::stderr().lock(), "{}", tail.join("\n")));
   }

   /// CXX=false, a working c++ would make the build compile a C++ test helper
   /// against our headers.
   fn configure(
      build: &Path,
      src: &Path,
      stage: &Stage,
      linux_headers: &str,
      rt: &str,
      sh: &str,
   ) -> io::Result<()> {
      let mut cc_words = vec!["clang".to_owned()];
      cc_words.extend(stage.target());
      cc_words.extend([
         format!("-resource-dir={rt}"),
         "--start-no-unused-arguments".to_owned(),
         "-rtlib=compiler-rt".to_owned(),
         "-unwindlib=none".to_owned(),
         "-fuse-ld=lld".to_owned(),
         "--end-no-unused-arguments".to_owned(),
      ]);
      let cc = cc_words.join(" ");
      let ldflags = format!("-L{rt}/lib/{}", stage.clang_target);
      let env: [(&str, &str); 13] = [
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
      ];
      let configure_script = src.join("configure").to_string_lossy().into_owned();
      let prefix_arg = format!("--prefix={PREFIX}");
      let host_arg = format!("--host={}", stage.clang_target);
      let build_arg = format!("--build={}", stage.build_triple);
      let headers_arg = format!("--with-headers={linux_headers}/include");
      let slibdir_arg = format!("libc_cv_slibdir={PREFIX}/lib");
      let rtlddir_arg = format!("libc_cv_rtlddir={PREFIX}/lib");
      let mut args = vec![
         configure_script.as_str(),
         &prefix_arg,
         "--sysconfdir=/etc",
         &host_arg,
         &build_arg,
         &headers_arg,
         "--enable-kernel=5.10",
         "--disable-werror",
         "--disable-nscd",
         "--enable-bind-now",
         "--enable-fortify-source",
         "--enable-stack-protector=strong",
         &slibdir_arg,
         &rtlddir_arg,
      ];
      args.extend_from_slice(Self::cpu_flags(&stage.cpu));
      if let Err(err) = run_in_env(Some(build), "sh", &args, &env) {
         Self::print_configure_log(build);
         return Err(err);
      }
      Ok(())
   }

   fn install_tree(dest: &Path, out: &str) -> io::Result<()> {
      create_dir_all(out)?;

      for entry in read_dir(dest.join(PREFIX.trim_start_matches('/')))? {
         let child = entry?;
         run("mv", &[&child.path().to_string_lossy(), out])?;
      }

      for script in files(&format!("{out}/bin/{{ldd,sotruss,xtrace,tzselect}}"))? {
         let text = read_to_string(&script)?.replace(PREFIX, "${0%/*}/..");
         write_file(&script, text.as_bytes(), None)?;
      }

      for library in files(&format!("{out}/lib/lib{{c,m}}.{{so,a}}"))? {
         let bytes = read(&library)?;
         if !bytes.starts_with(b"/* GNU ld script") {
            continue;
         }

         let text = String::from_utf8(bytes)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?
            .replace(&format!("{PREFIX}/lib/"), "");
         write_file(&library, text.as_bytes(), None)?;
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
      let file_err = file.try_clone()?;
      let status = Command::new("make")
         .args(args)
         .current_dir(build)
         .stdout(Stdio::from(file))
         .stderr(Stdio::from(file_err))
         .status()?;
      if !status.success() {
         let log = read_to_string(log_path)?;
         let mut output_lines = Vec::new();
         for line in log.lines() {
            let lower = line.to_ascii_lowercase();
            let has_error = lower.contains("error") || line.contains("***");
            let not_werror = !line.contains("Werror") && !line.contains("-Wno-error");
            if has_error && not_werror {
               output_lines.push(line);
            }
         }
         let noise_filtered: Vec<&str> = log
            .lines()
            .filter(|line| {
               !line.contains("reassign symbol")
                  && !line.contains("static-libgcc")
                  && !line.contains("overriding recipe")
                  && !line.contains("ignoring old recipe")
                  && !line.contains(" -c ")
            })
            .collect();
         let start = noise_filtered.len().saturating_sub(100);
         output_lines.extend_from_slice(&noise_filtered[start..]);
         drop(writeln!(io::stderr().lock(), "{}", output_lines.join("\n")));
         let target = args.last().copied().unwrap_or("target");
         return Err(io::Error::other(format!("glibc make {target} failed")));
      }
      Ok(())
   }
}

impl Recipe for Glibc {
   fn name(&self) -> &'static str {
      "glibc"
   }

   fn run(&self, stage: &Stage) -> io::Result<()> {
      let out = stage.out();
      let src = Self::patched_source(stage)?;
      let build = PathBuf::from(&stage.build_top).join("build");
      create_dir_all(&build)?;
      write_file(&build.join("configparms"), b"with-clang = yes\n", None)?;

      let rt = if let Some(compiler_rt) = stage.compiler_rt.as_deref() {
         compiler_rt.to_owned()
      } else {
         capture(&format!("{}/bin/clang", stage.llvm), &[
            "--print-resource-dir",
         ])?
         .trim()
         .to_owned()
      };
      let linux_headers = wanted(stage.linux_headers.as_deref(), "linuxHeaders")?;
      let sysincludes = format!("-nostdinc -isystem {rt}/include -isystem {linux_headers}/include");
      let sh = tool("sh")?.to_string_lossy().into_owned();
      Self::configure(&build, &src, stage, linux_headers, &rt, &sh)?;

      // Makeconfig's lazy $(shell $(CC) -print-file-name=libgcc_s.so.1) expands
      // ~600 times per build. There is no libgcc_s here with compiler-rt,
      // so the answer is empty.
      let cores_str = cores().to_string();
      let shell_arg = format!("SHELL={sh}");
      let sysincludes_arg = format!("sysincludes={sysincludes}");
      let make_base: [&str; 6] = [
         "-j",
         &cores_str,
         &shell_arg,
         &sysincludes_arg,
         "gnulib-extralibdir=",
         "zonedir=/usr/share/zoneinfo",
      ];
      let log_path = PathBuf::from(&stage.build_top).join("make.log");
      let dest = PathBuf::from(&stage.build_top).join("dest");
      let dest_arg = format!("DESTDIR={}", dest.display());

      if stage.headers_only() {
         let mut headers_args = Vec::from(make_base);
         headers_args.extend(["install-headers", &dest_arg]);
         Self::make_logged(&build, &headers_args, &log_path)?;
         Self::install_tree(&dest, out)?;
         let stubs = format!("{out}/include/gnu/stubs.h");
         if !Path::new(&stubs).exists() {
            write_file(Path::new(&stubs), b"", None)?;
         }
         cc_facts(out, &[("include-dirs", &["include"])])?;
         return Ok(());
      }

      Self::make_logged(&build, &make_base, &log_path)?;
      // Parallel install races on the .dt to .d depfile conversion because
      // several sub-makes include the same sysd-rules and each moves the
      // same files. Nothing is compiled here anyway.
      let mut install_args = Vec::from(make_base);
      install_args.extend(["install", "-j1", &dest_arg]);
      Self::make_logged(&build, &install_args, &log_path)?;
      Self::install_tree(&dest, out)?;

      // C.UTF-8 so LC_ALL=C.UTF-8 works everywhere without a locales package,
      // charmap data only, ~360 K.
      if stage.locale == Some(true) {
         let locale_dir = format!("{out}/lib/locale");
         create_dir_all(&locale_dir)?;
         let interp_path = format!("{out}/lib/{}", stage.interp);
         let lib_dir = format!("{out}/lib");
         let localedef_path = format!("{out}/bin/localedef");
         let locale_out = format!("{out}/lib/locale/C.utf8");
         let i18n_path = format!("{}/localedata", src.display());
         run_with(
            &interp_path,
            &[
               "--library-path",
               &lib_dir,
               &localedef_path,
               "--no-archive",
               "-i",
               "C",
               "-f",
               "UTF-8",
               &locale_out,
            ],
            &[("I18NPATH", &i18n_path)],
         )?;
      }

      let lib_count = read_dir(format!("{out}/lib"))?.count();
      cc_facts(out, &[("include-dirs", &["include"])])?;
      say(&format!("glibc {}: {lib_count} files in lib/", stage.cpu));
      Ok(())
   }
}
