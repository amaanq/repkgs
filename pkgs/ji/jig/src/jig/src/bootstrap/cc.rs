use std::{
   env::{
      consts::ARCH,
      set_current_dir,
   },
   fs::{
      copy,
      create_dir_all,
      read_to_string,
   },
   io,
   os::unix::fs::symlink,
   path::Path,
};

use jig::files::write_file;

use crate::bootstrap::{
   dispatch::Recipe,
   stage::{
      Stage,
      capture,
      cc_fact,
      run,
      say,
      strs,
      wanted,
   },
};

struct Driver {
   flags:    Vec<String>,
   cxxflags: String,
}

const STUB_LD: &str = "SECTIONS {
  . = 0;
  .text : { KEEP(*(.text.header)) *(.text.entry) *(.text .text.* .rodata .rodata.*) }
  /DISCARD/ : { *(.dynsym .dynstr .hash .gnu.hash .dynamic .interp .comment .note.* .eh_frame*) }
}
";

pub struct Cc;

impl Cc {
   // absolute seed paths, cc must work with an empty PATH, and clang on PATH
   // is the cache shim
   fn seed_bin(stage: &Stage, name: &str) -> String {
      format!("{}/bin/{name}", stage.seed)
   }

   fn lld(stage: &Stage) -> io::Result<String> {
      let name = match stage.binfmt.as_str() {
         "elf" => "ld.lld",
         "macho" => "ld64.lld",
         "coff" => "lld-link",
         other => return Err(io::Error::other(format!("unknown binfmt {other}"))),
      };
      Ok(Self::seed_bin(stage, name))
   }

   fn driver_flags(stage: &Stage, sysroot: &str) -> io::Result<Driver> {
      let lld = Self::lld(stage)?;
      let given = cc_fact(sysroot, "flags")?;
      let Some(facts) = given else {
         let mut flags: Vec<String> = stage
            .ccflags()?
            .into_iter()
            .filter(|flag| flag != "-unwindlib=none")
            .collect();
         flags.push("-unwindlib=libunwind".to_owned());
         flags.push(format!("--ld-path={lld}"));
         return Ok(Driver {
            flags,
            cxxflags: "-stdlib=libc++".to_owned(),
         });
      };
      let fill = |word: &str| -> String { word.replace("SYSROOT", sysroot).replace("LLD", &lld) };
      let mut flags = stage.target();
      flags.extend(facts.iter().map(|word| fill(word)));
      let cxx_facts = cc_fact(sysroot, "cxxflags")?.unwrap_or_default();
      let cxxflags = cxx_facts
         .iter()
         .map(|word| fill(word))
         .collect::<Vec<String>>()
         .join(" ");
      Ok(Driver { flags, cxxflags })
   }

   fn elf_policy(stage: &Stage, out: &str, sysroot: &str) -> io::Result<Vec<(String, String)>> {
      let crt_interp = wanted(stage.crt_interp.as_deref(), "crt_interp")?;
      // compile from cwd, the STT_FILE symbol would otherwise record a store
      // path
      copy(crt_interp, "crt_interp.c")?;
      let mut flags = stage.target();
      flags.extend([
         "-O2".to_owned(),
         "-fPIE".to_owned(),
         "-ffreestanding".to_owned(),
         "-nostdlib".to_owned(),
         "-nostdinc".to_owned(),
         "-fno-builtin".to_owned(),
         "-fno-stack-protector".to_owned(),
         "-fno-asynchronous-unwind-tables".to_owned(),
      ]);
      let mut crt_args = strs(&flags);
      let crt_out = format!("{out}/lib/crt_interp.o");
      crt_args.extend(["-c", "crt_interp.c", "-o", &crt_out]);
      run("clang", &crt_args)?;

      let mut reloc_args = strs(&flags);
      reloc_args.extend([
         "-DRELOC_STUB",
         "-fno-jump-tables",
         "-fvisibility=hidden",
         "-c",
         "crt_interp.c",
         "-o",
         "reloc_stub.o",
      ]);
      run("clang", &reloc_args)?;

      write_file(Path::new("stub.ld"), STUB_LD.as_bytes(), None)?;

      let lld_bin = Self::lld(stage)?;
      run(&lld_bin, &[
         "-pie",
         "--no-dynamic-linker",
         "-e",
         "__reloc_start",
         "-T",
         "stub.ld",
         "reloc_stub.o",
         "-o",
         "reloc_stub.elf",
      ])?;

      let reloc_bin = format!("{out}/lib/reloc_stub.bin");
      run("llvm-objcopy", &[
         "-O",
         "binary",
         "-j",
         ".text",
         "reloc_stub.elf",
         &reloc_bin,
      ])?;

      Ok(vec![
         ("libc".to_owned(), sysroot.to_owned()),
         ("interp".to_owned(), stage.interp.clone()),
         ("crt".to_owned(), crt_out),
         ("runtimes".to_owned(), format!("{sysroot}/lib")),
      ])
   }

   fn smoke_test(stage: &Stage, out: &str) -> io::Result<()> {
      let exe = if stage.binfmt == "coff" { ".exe" } else { "" };
      let hello_c = "#include <stdio.h>\nint main(void) { puts(\"cc ok\"); }\n";
      write_file(Path::new("hello.c"), hello_c.as_bytes(), None)?;
      let hello_cc = "#include <print>\nint main() { std::println(\"c++ ok\"); }\n";
      write_file(Path::new("hello.cc"), hello_cc.as_bytes(), None)?;

      let cc_bin = format!("{out}/bin/cc");
      let hello_out = format!("hello{exe}");
      run(&cc_bin, &["hello.c", "-o", &hello_out])?;

      let cxx_bin = format!("{out}/bin/c++");
      let hellocxx_out = format!("hello++{exe}");
      run(&cxx_bin, &["-std=c++23", "hello.cc", "-o", &hellocxx_out])?;

      if stage.os == "linux" && stage.cpu == ARCH {
         let first = capture("./hello", &[])?;
         let second = capture("./hello++", &[])?;
         say(&format!("{first}{second}"));
      }
      Ok(())
   }
}

impl Recipe for Cc {
   fn name(&self) -> &'static str {
      "cc"
   }

   fn run(&self, stage: &Stage) -> io::Result<()> {
      let out = stage.out();
      let sysroot = wanted(stage.sysroot.as_deref(), "sysroot")?;
      create_dir_all(format!("{out}/bin"))?;
      create_dir_all(format!("{out}/lib"))?;
      create_dir_all(format!("{out}/etc"))?;
      set_current_dir(&stage.build_top)?;

      let prebuilt = wanted(stage.prebuilt.as_deref(), "prebuilt")?;
      copy(format!("{prebuilt}/bin/jig"), format!("{out}/bin/jig"))?;

      // jig under every name a build system may call. No clang alias, that name
      // keeps meaning the raw seed compiler, which bootstrap recipes
      // drive themselves
      for name in [
         "cc",
         "c++",
         "gcc",
         "g++",
         "reloc-fixup",
         "gocacheprog",
         "rustcwrap",
      ] {
         symlink("jig", format!("{out}/bin/{name}"))?;
      }

      // lld picks its personality from argv[0], and "ld" means ELF, spell the
      // flavour out elsewhere
      if stage.binfmt == "elf" {
         symlink(Self::lld(stage)?, format!("{out}/bin/ld"))?;
      } else {
         let flavor = match stage.binfmt.as_str() {
            "macho" => "darwin",
            "coff" => "link",
            other => return Err(io::Error::other(format!("unknown binfmt {other}"))),
         };
         let script = format!(
            "#!/bin/sh\nexec {} -flavor {flavor} \"$@\"\n",
            Self::seed_bin(stage, "lld")
         );
         write_file(
            Path::new(&format!("{out}/bin/ld")),
            script.as_bytes(),
            Some(0o755),
         )?;
      }

      // CC_FOR_BUILD when cross, jig locates its conf via /proc/self/exe
      if let Some(native) = stage.native.as_deref() {
         for name in ["cc", "c++"] {
            symlink(
               format!("{native}/bin/{name}"),
               format!("{out}/bin/{name}-build"),
            )?;
         }
      }

      // etc/roots, store dirs a depfile can name such as sysroot members and
      // seed resource headers, passed to the compile cache as
      // JIG_STORE_ROOTS
      let roots_file = format!("{sysroot}/roots");
      let members = if Path::new(&roots_file).exists() {
         read_to_string(&roots_file)?.trim().to_owned()
      } else {
         String::new()
      };
      let roots_text = format!("{sysroot} {} {members}\n", stage.seed);
      write_file(
         Path::new(&format!("{out}/etc/roots")),
         roots_text.as_bytes(),
         None,
      )?;

      let driver = Self::driver_flags(stage, sysroot)?;

      let include = format!("{out}/include");
      create_dir_all(&include)?;
      let reloc = wanted(stage.reloc.as_deref(), "reloc")?;
      copy(format!("{reloc}/reloc.h"), format!("{include}/reloc.h"))?;
      copy(
         format!("{reloc}/reloc_self_{}.h", stage.os),
         format!("{include}/reloc_self.h"),
      )?;

      let policy = if stage.binfmt == "elf" {
         Self::elf_policy(stage, out, sysroot)?
      } else {
         Vec::new()
      };

      // -B, cc -print-prog-name=ld for libtool's with_gnu_ld probe answers
      // bin/ld, the target's lld flavour, not the ELF ld.lld beside the
      // seed clang
      let mut flags = vec![format!("-B{out}/bin"), format!("-isystem{out}/include")];
      flags.extend(driver.flags);

      let mut conf: Vec<(&str, String)> = vec![
         ("cc", Self::seed_bin(stage, "clang")),
         ("binfmt", stage.binfmt.clone()),
         ("flags", flags.join(" ")),
         ("cxxflags", driver.cxxflags),
         ("prefix-map", format!("{sysroot}=/sysroot:{out}=/cc")),
      ];
      for entry in &policy {
         conf.push((entry.0.as_str(), entry.1.clone()));
      }
      let lines: Vec<String> = conf
         .into_iter()
         .map(|entry| format!("{} = {}", entry.0, entry.1))
         .collect();
      let mut conf_text = lines.join("\n");
      conf_text.push('\n');
      write_file(
         Path::new(&format!("{out}/etc/jig.conf")),
         conf_text.as_bytes(),
         None,
      )?;

      Self::smoke_test(stage, out)
   }
}
