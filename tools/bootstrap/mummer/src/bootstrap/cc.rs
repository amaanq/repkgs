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
   path::{
      Path,
      PathBuf,
   },
   process::Command,
};

use mummer::{
   attrs::load,
   config::{
      BinaryFormat,
      Config,
      ElfPolicy,
   },
   files::write_file,
};
use serde::Deserialize;

use crate::bootstrap::{
   dispatch::Recipe,
   stage::{
      Stage,
      capture,
      cc_fact,
      execute,
      say,
   },
   target::{
      Os,
      Profile,
   },
};

#[derive(Deserialize)]
struct Attrs {
   prebuilt:   PathBuf,
   reloc:      PathBuf,
   #[serde(default)]
   crt_interp: Option<PathBuf>,
   #[serde(default)]
   native:     Option<PathBuf>,
}

struct Driver {
   flags:    Vec<String>,
   cxxflags: Vec<String>,
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
   fn llvm_bin(stage: &Stage, name: &str) -> PathBuf {
      stage.llvm.join("bin").join(name)
   }

   fn lld(stage: &Stage) -> PathBuf {
      Self::llvm_bin(stage, stage.profile.lld())
   }

   fn script(path: &Path, command: &str) -> io::Result<()> {
      write_file(
         path,
         format!("#!/bin/sh\nexec {command} \"$@\"\n").as_bytes(),
         Some(0o755),
      )
   }

   fn linker_tools(stage: &Stage, sysroot: &Path) -> io::Result<()> {
      let bin = stage.out.join("bin");
      // lld picks its personality from argv[0], and "ld" means ELF, spell the
      // flavour out elsewhere
      let ldargs = match (stage.profile.ld_flavor(), stage.profile) {
         (Some(flavor), _) => {
            Some(format!(
               "{} -flavor {flavor}",
               Self::llvm_bin(stage, "lld").display()
            ))
         },
         (None, Profile::Mingw(machine)) => {
            Some(format!(
               "{} -m {}",
               Self::lld(stage).display(),
               machine.emulation()
            ))
         },
         (None, Profile::Elf | Profile::Macho | Profile::Msvc) => None,
      };
      match ldargs {
         Some(args) => Self::script(&bin.join("ld"), &args)?,
         None => symlink(Self::lld(stage), bin.join("ld"))?,
      }
      if let Profile::Mingw(machine) = stage.profile {
         Self::script(
            &bin.join("windres"),
            &format!(
               "{} --target={} --preprocessor-arg=--sysroot={}",
               Self::llvm_bin(stage, "llvm-windres").display(),
               machine.bfd(),
               sysroot.display()
            ),
         )?;
         Self::script(
            &bin.join("dlltool"),
            &format!(
               "{} -m {}",
               Self::llvm_bin(stage, "llvm-dlltool").display(),
               machine.dlltool()
            ),
         )?;
      }
      Ok(())
   }

   fn driver_flags(stage: &Stage, sysroot: &Path) -> io::Result<Driver> {
      let lld = Self::lld(stage).display().to_string();
      let Some(facts) = cc_fact(sysroot, "flags")? else {
         let mut flags = stage
            .ccflags()?
            .into_iter()
            .filter(|flag| flag != "-unwindlib=none")
            .collect::<Vec<_>>();
         flags.push("-unwindlib=libunwind".to_owned());
         flags.push(format!("--ld-path={lld}"));
         return Ok(Driver {
            flags,
            cxxflags: vec!["-stdlib=libc++".to_owned()],
         });
      };
      let sysroot_text = sysroot.display().to_string();
      let fill = |word: &String| word.replace("SYSROOT", &sysroot_text).replace("LLD", &lld);
      let mut flags = stage.target();
      flags.extend(facts.iter().map(fill));
      let cxxflags = cc_fact(sysroot, "cxxflags")?
         .unwrap_or_default()
         .iter()
         .map(fill)
         .collect();
      Ok(Driver { flags, cxxflags })
   }

   /// The entry stub every ELF executable starts in, compiled from cwd so its
   /// `STT_FILE` symbol records no store path.
   fn elf_policy(stage: &Stage, attrs: &Attrs, sysroot: &Path) -> io::Result<ElfPolicy> {
      let crt_interp = attrs
         .crt_interp
         .as_deref()
         .ok_or_else(|| io::Error::other("this stage has no crt_interp"))?;
      copy(crt_interp, "crt_interp.c")?;
      let mut flags = stage.target();
      flags.extend(
         [
            "-O2",
            "-fPIE",
            "-ffreestanding",
            "-nostdlib",
            "-nostdinc",
            "-fno-builtin",
            "-fno-stack-protector",
            "-fno-asynchronous-unwind-tables",
         ]
         .map(str::to_owned),
      );
      let crt = stage.out.join("lib/crt_interp.o");
      execute(
         Command::new("clang")
            .args(&flags)
            .args(["-c", "crt_interp.c", "-o"])
            .arg(&crt),
      )?;
      execute(Command::new("clang").args(&flags).args([
         "-DRELOC_STUB",
         "-fno-jump-tables",
         "-fvisibility=hidden",
         "-c",
         "crt_interp.c",
         "-o",
         "reloc_stub.o",
      ]))?;
      write_file(Path::new("stub.ld"), STUB_LD.as_bytes(), None)?;
      execute(Command::new(Self::lld(stage)).args([
         "-pie",
         "--no-dynamic-linker",
         "-e",
         "__reloc_start",
         "-T",
         "stub.ld",
         "reloc_stub.o",
         "-o",
         "reloc_stub.elf",
      ]))?;
      execute(
         Command::new("llvm-objcopy")
            .args(["-O", "binary", "-j", ".text", "reloc_stub.elf"])
            .arg(stage.out.join("lib/reloc_stub.bin")),
      )?;
      Ok(ElfPolicy {
         libc:   text(sysroot.to_path_buf())?,
         interp: stage.interp.clone(),
         crt:    Some(text(crt)?),
      })
   }

   fn smoke_test(stage: &Stage) -> io::Result<()> {
      let exe = stage.profile.exe();
      let hello_c = "#include <stdio.h>\nint main(void) { puts(\"cc ok\"); }\n";
      write_file(Path::new("hello.c"), hello_c.as_bytes(), None)?;
      let hello_cc = "#include <print>\nint main() { std::println(\"c++ ok\"); }\n";
      write_file(Path::new("hello.cc"), hello_cc.as_bytes(), None)?;
      execute(
         Command::new(stage.out.join("bin/cc"))
            .args(["hello.c", "-o"])
            .arg(format!("hello{exe}")),
      )?;
      execute(
         Command::new(stage.out.join("bin/c++"))
            .args(["-std=c++23", "hello.cc", "-o"])
            .arg(format!("hello++{exe}")),
      )?;
      if stage.os == Os::Linux && stage.cpu.name() == ARCH {
         let first = capture(&mut Command::new("./hello"))?;
         let second = capture(&mut Command::new("./hello++"))?;
         say(&format!("{first}{second}"));
      }
      Ok(())
   }

   /// etc/roots, store dirs a depfile can name such as sysroot members and
   /// compiler resource headers, passed to the compile cache as
   /// `MUMMER_STORE_ROOTS`.
   fn roots(stage: &Stage, sysroot: &Path) -> io::Result<()> {
      let members = match read_to_string(sysroot.join("roots")) {
         Ok(text) => text.trim().to_owned(),
         Err(error) if error.kind() == io::ErrorKind::NotFound => String::new(),
         Err(error) => return Err(error),
      };
      write_file(
         &stage.out.join("etc/roots"),
         format!("{} {} {members}\n", sysroot.display(), stage.llvm.display()).as_bytes(),
         None,
      )
   }
}

impl Recipe for Cc {
   fn name(&self) -> &'static str {
      "cc"
   }

   fn run(&self, stage: &Stage) -> io::Result<()> {
      let attrs = load::<Attrs>()?;
      let out = &stage.out;
      let sysroot = stage.sysroot()?;
      for directory in ["bin", "lib", "etc"] {
         create_dir_all(out.join(directory))?;
      }
      set_current_dir(&stage.build_top)?;
      copy(attrs.prebuilt.join("bin/mummer"), out.join("bin/mummer"))?;

      // mummer under every name a build system may call. No clang alias, that
      // name keeps meaning the raw compiler, which bootstrap recipes
      // drive themselves
      for name in [
         "cc",
         "c++",
         "gcc",
         "g++",
         "cpp",
         "reloc-fixup",
         "gocacheprog",
         "rustcwrap",
         "ghcwrap",
      ] {
         symlink("mummer", out.join("bin").join(name))?;
      }
      Self::linker_tools(stage, sysroot)?;

      // CC_FOR_BUILD when cross, mummer locates its conf via /proc/self/exe
      if let Some(ref native) = attrs.native {
         for name in ["cc", "c++"] {
            symlink(
               native.join("bin").join(name),
               out.join(format!("bin/{name}-build")),
            )?;
         }
      }
      Self::roots(stage, sysroot)?;
      let driver = Self::driver_flags(stage, sysroot)?;

      let include = out.join("include");
      create_dir_all(&include)?;
      copy(attrs.reloc.join("reloc.h"), include.join("reloc.h"))?;
      copy(
         attrs
            .reloc
            .join(format!("reloc_self_{}.h", stage.os.name())),
         include.join("reloc_self.h"),
      )?;
      let policy = if stage.binfmt == BinaryFormat::Elf {
         Some(Self::elf_policy(stage, &attrs, sysroot)?)
      } else {
         None
      };

      // -B, cc -print-prog-name=ld for libtool's with_gnu_ld probe answers
      // bin/ld, the target's lld flavour, not the ELF ld.lld beside the
      // raw clang
      let mut flags = vec![
         format!("-B{}/bin", out.display()),
         format!("-isystem{}/include", out.display()),
      ];
      flags.extend(driver.flags);
      let config = Config {
         cc: text(Self::llvm_bin(stage, "clang"))?,
         binfmt: stage.binfmt,
         flags,
         cxxflags: driver.cxxflags,
         prefix_map: vec![
            format!("{}=/sysroot", sysroot.display()),
            format!("{}=/cc", out.display()),
         ],
         policy,
         fc: None,
         fflags: Vec::new(),
      };
      write_file(
         &out.join("etc/mummer.json"),
         &serde_json::to_vec_pretty(&config)?,
         None,
      )?;
      Self::smoke_test(stage)
   }
}

/// A path as `mummer.json` holds it, which must be UTF-8.
fn text(path: PathBuf) -> io::Result<String> {
   path
      .into_os_string()
      .into_string()
      .map_err(|raw| io::Error::other(format!("{} is not UTF-8", raw.display())))
}
