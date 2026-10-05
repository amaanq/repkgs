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

use mummer::files::write_file;
use serde::Serialize;

use crate::bootstrap::{
   dispatch::Recipe,
   stage::{
      Stage,
      TargetProfile,
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
   cxxflags: Vec<String>,
}

#[derive(Serialize)]
struct Config<'stage> {
   cc:         String,
   binfmt:     &'stage str,
   flags:      Vec<String>,
   cxxflags:   Vec<String>,
   #[serde(rename = "prefix-map")]
   prefix_map: Vec<String>,
   #[serde(flatten, skip_serializing_if = "Option::is_none")]
   policy:     Option<ElfPolicy<'stage>>,
}

#[derive(Serialize)]
struct ElfPolicy<'stage> {
   libc:   &'stage str,
   interp: &'stage str,
   crt:    String,
}

const STUB_LD: &str = "SECTIONS {
  . = 0;
  .text : { KEEP(*(.text.header)) *(.text.entry) *(.text .text.* .rodata .rodata.*) }
  /DISCARD/ : { *(.dynsym .dynstr .hash .gnu.hash .dynamic .interp .comment .note.* .eh_frame*) }
}
";

pub struct Cc;

impl Cc {
   // absolute compiler paths, cc must work with an empty PATH, and clang on
   // PATH is the cache mummer
   fn llvm_bin(stage: &Stage, name: &str) -> String {
      format!("{}/bin/{name}", stage.llvm)
   }

   fn lld(stage: &Stage) -> io::Result<String> {
      Ok(Self::llvm_bin(stage, stage.profile()?.lld()))
   }

   fn linker_tools(stage: &Stage, out: &str, sysroot: &str) -> io::Result<()> {
      // lld picks its personality from argv[0], and "ld" means ELF, spell the
      // flavour out elsewhere
      let profile = stage.profile()?;
      let ldargs = if let Some(flavor) = profile.ld_flavor() {
         Some(format!("{} -flavor {flavor}", Self::llvm_bin(stage, "lld")))
      } else if let TargetProfile::Mingw(machine) = profile {
         Some(format!("{} -m {}", Self::lld(stage)?, machine.emulation()))
      } else {
         None
      };
      if let Some(args) = ldargs {
         let script = format!("#!/bin/sh\nexec {args} \"$@\"\n");
         write_file(
            Path::new(&format!("{out}/bin/ld")),
            script.as_bytes(),
            Some(0o755),
         )?;
      } else {
         symlink(Self::lld(stage)?, format!("{out}/bin/ld"))?;
      }
      if let TargetProfile::Mingw(machine) = profile {
         let windres = format!(
            "#!/bin/sh\nexec {} --target={} --preprocessor-arg=--sysroot={sysroot} \"$@\"\n",
            Self::llvm_bin(stage, "llvm-windres"),
            machine.bfd(),
         );
         let dlltool = format!(
            "#!/bin/sh\nexec {} -m {} \"$@\"\n",
            Self::llvm_bin(stage, "llvm-dlltool"),
            machine.dlltool(),
         );
         for (name, script) in [("windres", windres), ("dlltool", dlltool)] {
            write_file(
               Path::new(&format!("{out}/bin/{name}")),
               script.as_bytes(),
               Some(0o755),
            )?;
         }
      }
      Ok(())
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
            cxxflags: vec!["-stdlib=libc++".to_owned()],
         });
      };
      let fill = |word: &str| -> String { word.replace("SYSROOT", sysroot).replace("LLD", &lld) };
      let mut flags = stage.target();
      flags.extend(facts.iter().map(|word| fill(word)));
      let cxx_facts = cc_fact(sysroot, "cxxflags")?.unwrap_or_default();
      let cxxflags = cxx_facts.iter().map(|word| fill(word)).collect();
      Ok(Driver { flags, cxxflags })
   }

   fn elf_policy<'stage>(
      stage: &'stage Stage,
      out: &str,
      sysroot: &'stage str,
   ) -> io::Result<ElfPolicy<'stage>> {
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

      Ok(ElfPolicy {
         libc:   sysroot,
         interp: &stage.interp,
         crt:    crt_out,
      })
   }

   fn smoke_test(stage: &Stage, out: &str) -> io::Result<()> {
      let exe = stage.profile()?.exe();
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
      copy(
         format!("{prebuilt}/bin/mummer"),
         format!("{out}/bin/mummer"),
      )?;

      // mummer under every name a build system may call. No clang alias, that
      // name keeps meaning the raw compiler, which bootstrap recipes
      // drive themselves
      for name in [
         "cc",
         "c++",
         "gcc",
         "g++",
         "reloc-fixup",
         "gocacheprog",
         "rustcwrap",
         "ghcwrap",
      ] {
         symlink("mummer", format!("{out}/bin/{name}"))?;
      }

      Self::linker_tools(stage, out, sysroot)?;

      // CC_FOR_BUILD when cross, mummer locates its conf via /proc/self/exe
      if let Some(native) = stage.native.as_deref() {
         for name in ["cc", "c++"] {
            symlink(
               format!("{native}/bin/{name}"),
               format!("{out}/bin/{name}-build"),
            )?;
         }
      }

      // etc/roots, store dirs a depfile can name such as sysroot members and
      // compiler resource headers, passed to the compile cache as
      // MUMMER_STORE_ROOTS
      let roots_file = format!("{sysroot}/roots");
      let members = if Path::new(&roots_file).exists() {
         read_to_string(&roots_file)?.trim().to_owned()
      } else {
         String::new()
      };
      let roots_text = format!("{sysroot} {} {members}\n", stage.llvm);
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
         Some(Self::elf_policy(stage, out, sysroot)?)
      } else {
         None
      };

      // -B, cc -print-prog-name=ld for libtool's with_gnu_ld probe answers
      // bin/ld, the target's lld flavour, not the ELF ld.lld beside the
      // raw clang
      let mut flags = vec![format!("-B{out}/bin"), format!("-isystem{out}/include")];
      flags.extend(driver.flags);

      let conf = Config {
         cc: Self::llvm_bin(stage, "clang"),
         binfmt: &stage.binfmt,
         flags,
         cxxflags: driver.cxxflags,
         prefix_map: vec![format!("{sysroot}=/sysroot"), format!("{out}=/cc")],
         policy,
      };
      write_file(
         Path::new(&format!("{out}/etc/mummer.json")),
         &serde_json::to_vec_pretty(&conf)?,
         None,
      )?;

      Self::smoke_test(stage, out)
   }
}
