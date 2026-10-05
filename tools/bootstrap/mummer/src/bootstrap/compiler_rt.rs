use std::{
   fs::create_dir_all,
   io,
   os::unix::fs::symlink,
   path::{
      Path,
      PathBuf,
   },
   process::Command,
};

use mummer::attrs::load;
use serde::Deserialize;

use crate::bootstrap::{
   dispatch::Recipe,
   stage::{
      Item,
      Stage,
      archive,
      capture,
      cc_fact,
      compile,
      copy_tree,
      files,
      read_list,
      say,
   },
   target::{
      Binfmt,
      Cpu,
   },
};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Attrs {
   libc_headers:  PathBuf,
   list:          PathBuf,
   #[serde(default)]
   linux_headers: Option<PathBuf>,
}

pub struct CompilerRt;

impl CompilerRt {
   /// only the libc headers exist at this point, libc itself links against
   /// what is built here. The libc names its header dirs in
   /// etc/cc/include-dirs, and a case-insensitive header overlay
   /// (windows-sdk) rides in its flags relative to its own location.
   fn libc_includes(attrs: &Attrs) -> io::Result<Vec<String>> {
      let libc = &attrs.libc_headers;
      let mut dirs = cc_fact(libc, "include-dirs")?
         .unwrap_or_default()
         .iter()
         .map(|dir| libc.join(dir).display().to_string())
         .collect::<Vec<_>>();
      if let Some(ref linux) = attrs.linux_headers {
         dirs.push(linux.join("include").display().to_string());
      }
      let libc_text = libc.display().to_string();
      let flags = cc_fact(libc, "flags")?
         .unwrap_or_default()
         .iter()
         .map(|flag| flag.replace("SYSROOT", &libc_text))
         .collect::<Vec<_>>();
      let mut args = vec!["-nostdlibinc".to_owned()];
      for dir in dirs {
         args.extend(["-isystem".to_owned(), dir]);
      }
      for pair in flags.windows(2) {
         if let [ref flag, ref overlay] = *pair
            && flag == "-ivfsoverlay"
         {
            args.extend(["-ivfsoverlay".to_owned(), overlay.clone()]);
         }
      }
      Ok(args)
   }

   /// "@`lse/outline_atomic`_<op><size>_<model>.S" stands for aarch64/lse.S
   /// built with those three defines, cmake generates the same by symlinking.
   fn builtin_item(builtins: &Path, obj: &Path, file: &str) -> Item {
      if let Some(rest) = file
         .strip_prefix("@lse/outline_atomic_")
         .and_then(|rest| rest.strip_suffix(".S"))
         && let Some((op_size, model)) = rest.rsplit_once('_')
      {
         let split = op_size
            .find(|ch: char| ch.is_ascii_digit())
            .unwrap_or(op_size.len());
         let (op, size) = op_size.split_at(split);
         return Item {
            src:   builtins.join("aarch64/lse.S"),
            obj:   obj.join(format!("{file}.o")),
            flags: vec![
               format!("-DL_{op}"),
               format!("-DSIZE={size}"),
               format!("-DMODEL={model}"),
            ],
         };
      }
      let std: &[&str] = if Path::new(file)
         .extension()
         .is_some_and(|extension| extension.eq_ignore_ascii_case("cpp"))
      {
         &["-std=c++17", "-fno-exceptions", "-fno-rtti", "-nostdinc++"]
      } else {
         &["-std=gnu11"]
      };
      Item {
         src:   builtins.join(file),
         obj:   obj.join(format!("{file}.o")),
         flags: std.iter().copied().map(str::to_owned).collect(),
      }
   }

   /// ELF only, crtbegin/crtend (vcruntime and libSystem bring their own)
   /// and GCC's crt names for glibc's Makeconfig, which links them even when
   /// configure saw compiler-rt.
   fn elf_extras(stage: &Stage, builtins: &Path, common: &[String]) -> io::Result<()> {
      let libdir = stage.out.join("lib").join(&stage.clang_target);
      let crtflags = ["-DCRT_HAS_INITFINI_ARRAY", "-DEH_USE_FRAME_REGISTRY"].map(str::to_owned);
      compile("clang", common, &[
         Item {
            src:   builtins.join("crtbegin.c"),
            obj:   libdir.join("clang_rt.crtbegin.o"),
            flags: crtflags.to_vec(),
         },
         Item {
            src:   builtins.join("crtend.c"),
            obj:   libdir.join("clang_rt.crtend.o"),
            flags: crtflags.to_vec(),
         },
      ])?;
      for name in ["crtbegin.o", "crtbeginS.o", "crtbeginT.o"] {
         symlink("clang_rt.crtbegin.o", libdir.join(name))?;
      }
      for name in ["crtend.o", "crtendS.o"] {
         symlink("clang_rt.crtend.o", libdir.join(name))?;
      }
      Ok(())
   }

   /// runtime for -coverage and -fprofile-instr-generate, where clang looks
   /// for it per binfmt. On ELF it needs kernel headers (mmap flags), so
   /// stage1 only. *`ROCm`* is the separate `clang_rt.profile_rocm`,
   /// `WindowsMMap` the win32 mmap port.
   fn profile_runtime(stage: &Stage, attrs: &Attrs, src: &Path) -> io::Result<()> {
      if stage.binfmt == Binfmt::Elf && attrs.linux_headers.is_none() {
         return Ok(());
      }
      let profile = src.join("compiler-rt/lib/profile");
      let coff = stage.binfmt == Binfmt::Coff;
      let named = |path: &Path| {
         path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default()
      };
      let mut flags = stage.target();
      flags.extend(Self::libc_includes(attrs)?);
      flags.extend(["-O2", "-nostdinc++", "-w"].map(str::to_owned));
      flags.push(format!("-I{}/compiler-rt/include", src.display()));
      flags.push(format!("-I{}", profile.display()));
      flags.push("-DCOMPILER_RT_HAS_ATOMICS=1".to_owned());
      flags.extend(stage.profile.pic().iter().copied().map(str::to_owned));
      if stage.profile.posix() {
         flags.extend(
            [
               "-DCOMPILER_RT_HAS_FCNTL_LCK=1",
               "-DCOMPILER_RT_HAS_FLOCK=1",
               "-DCOMPILER_RT_HAS_UNAME=1",
            ]
            .map(str::to_owned),
         );
      }
      let items = files(&format!("{}/*.{{c,cpp}}", profile.display()))?
         .into_iter()
         .filter(|path| {
            let name = named(path);
            !(name.contains("ROCm") || (!coff && name.starts_with("WindowsMMap")))
         })
         .map(|source| {
            Item {
               obj:   stage
                  .build_top
                  .join(format!("obj/profile/{}.o", named(&source))),
               src:   source,
               flags: Vec::new(),
            }
         })
         .collect::<Vec<_>>();
      let objs = compile("clang", &flags, &items)?;
      archive(&stage.out.join(stage.runtime("profile")), &objs)
   }
}

impl Recipe for CompilerRt {
   fn name(&self) -> &'static str {
      "compiler-rt"
   }

   /// compiler-rt without cmake, the builtins from the per-target file list
   /// (pkgs/ll/llvm/builtins.toml, written by its update.nu) with the flags
   /// cmake would use, laid out as a clang resource dir. No
   /// -`DCOMPILER_RT_HAS_FLOAT16` on ppc, clang has no _Float16 there.
   fn run(&self, stage: &Stage) -> io::Result<()> {
      let attrs = load::<Attrs>()?;
      let src = stage.unpack("llvm", &["compiler-rt", "third-party/siphash"])?;
      let builtins = src.join("compiler-rt/lib/builtins");
      let obj = stage.build_top.join("obj");
      let percpu: &[&str] = match stage.cpu {
         Cpu::Powerpc64le => &[],
         Cpu::Aarch64 => {
            &[
               "-DCOMPILER_RT_HAS_FLOAT16",
               "-DENABLE_BAREMETAL_AARCH64_FMV",
               "-DHAS_ASM_LSE",
            ]
         },
         Cpu::X86_64 | Cpu::Riscv64 | Cpu::Loongarch64 => &["-DCOMPILER_RT_HAS_FLOAT16"],
      };
      let mut common = stage.target();
      common.extend(Self::libc_includes(&attrs)?);
      common.extend(stage.profile.pic().iter().copied().map(str::to_owned));
      common.extend(percpu.iter().copied().map(str::to_owned));
      common.extend(
         [
            "-O2",
            "-fno-builtin",
            "-fno-lto",
            "-fvisibility=hidden",
            "-fomit-frame-pointer",
            "-ffreestanding",
            "-DVISIBILITY_HIDDEN",
         ]
         .map(str::to_owned),
      );
      common.push(format!("-I{}", builtins.display()));
      common.push(format!("-I{}/third-party/siphash/include", src.display()));

      let items = read_list(&attrs.list)?
         .iter()
         .map(|file| Self::builtin_item(&builtins, &obj, file))
         .collect::<Vec<_>>();
      say(&format!(
         "compiler-rt builtins {}: {} objects",
         stage.cpu,
         items.len()
      ));
      let lib = stage.out.join(stage.runtime("builtins"));
      if let Some(parent) = lib.parent() {
         create_dir_all(parent)?;
      }
      let objs = compile("clang", &common, &items)?;
      archive(&lib, &objs)?;

      let resource =
         capture(Command::new(stage.llvm.join("bin/clang")).arg("--print-resource-dir"))?;
      copy_tree(
         &Path::new(resource.trim()).join("include"),
         &stage.out.join("include"),
         "**/*",
      )?;
      if stage.binfmt == Binfmt::Elf {
         Self::elf_extras(stage, &builtins, &common)?;
      }
      Self::profile_runtime(stage, &attrs, &src)
   }
}
