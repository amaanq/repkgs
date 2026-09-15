use std::{
   fs::create_dir_all,
   io,
   os::unix::fs::symlink,
   path::{
      Path,
      PathBuf,
   },
};

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
      wanted,
   },
};

pub struct CompilerRt;

impl CompilerRt {
   /// only the libc headers exist at this point, libc itself links against
   /// what is built here. The libc names its header dirs in
   /// etc/cc/include-dirs, and a case-insensitive header overlay
   /// (windows-sdk) rides in its flags relative to its own location.
   fn libc_includes(stage: &Stage) -> io::Result<Vec<String>> {
      let libc = wanted(stage.libc_headers.as_deref(), "libcHeaders")?;
      let mut dirs: Vec<String> = cc_fact(libc, "include-dirs")?
         .unwrap_or_default()
         .iter()
         .map(|dir| format!("{libc}/{dir}"))
         .collect();
      if let Some(linux) = stage.linux_headers.as_deref() {
         dirs.push(format!("{linux}/include"));
      }
      let flags: Vec<String> = cc_fact(libc, "flags")?
         .unwrap_or_default()
         .iter()
         .map(|flag| flag.replace("SYSROOT", libc))
         .collect();
      let mut args = vec!["-nostdlibinc".to_owned()];
      for dir in &dirs {
         args.push("-isystem".to_owned());
         args.push(dir.clone());
      }
      for (index, flag) in flags.iter().enumerate() {
         if flag == "-ivfsoverlay"
            && let Some(overlay) = flags.get(index + 1)
         {
            args.push("-ivfsoverlay".to_owned());
            args.push(overlay.clone());
         }
      }
      Ok(args)
   }

   /// "@`lse/outline_atomic`_<op><size>_<model>.S" stands for aarch64/lse.S
   /// built with those three defines, cmake generates the same by symlinking.
   fn builtin_item(builtins: &str, obj: &str, file: &str) -> Item {
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
            src:   PathBuf::from(format!("{builtins}/aarch64/lse.S")),
            obj:   PathBuf::from(format!("{obj}/{file}.o")),
            flags: vec![
               format!("-DL_{op}"),
               format!("-DSIZE={size}"),
               format!("-DMODEL={model}"),
            ],
         };
      }
      let std = if Path::new(file)
         .extension()
         .is_some_and(|extension| extension.eq_ignore_ascii_case("cpp"))
      {
         vec!["-std=c++17", "-fno-exceptions", "-fno-rtti", "-nostdinc++"]
      } else {
         vec!["-std=gnu11"]
      };
      Item {
         src:   PathBuf::from(format!("{builtins}/{file}")),
         obj:   PathBuf::from(format!("{obj}/{file}.o")),
         flags: std.into_iter().map(str::to_owned).collect(),
      }
   }

   /// where each driver looks for the builtins, the per-target runtime dir
   /// with lib<name>.a (ELF) or <name>.lib (lld-link), and for darwin only
   /// ever `lib/darwin/libclang_rt.osx.a`, fat in Xcode and one arch here.
   fn builtins_lib(stage: &Stage, out: &str) -> io::Result<String> {
      Ok(format!("{out}/{}", stage.profile()?.runtime("builtins")))
   }

   /// ELF only, crtbegin/crtend (vcruntime and libSystem bring their own)
   /// and GCC's crt names for glibc's Makeconfig, which links them even when
   /// configure saw compiler-rt.
   fn elf_extras(stage: &Stage, src: &Path, out: &str, common: &[String]) -> io::Result<()> {
      let builtins = format!("{}/compiler-rt/lib/builtins", src.display());
      let libdir = format!("{out}/lib/{}", stage.clang_target);
      let crtflags: Vec<String> = ["-DCRT_HAS_INITFINI_ARRAY", "-DEH_USE_FRAME_REGISTRY"]
         .into_iter()
         .map(str::to_owned)
         .collect();
      compile(common, &[
         Item {
            src:   PathBuf::from(format!("{builtins}/crtbegin.c")),
            obj:   PathBuf::from(format!("{libdir}/clang_rt.crtbegin.o")),
            flags: crtflags.clone(),
         },
         Item {
            src:   PathBuf::from(format!("{builtins}/crtend.c")),
            obj:   PathBuf::from(format!("{libdir}/clang_rt.crtend.o")),
            flags: crtflags,
         },
      ])?;
      for name in ["crtbegin.o", "crtbeginS.o", "crtbeginT.o"] {
         symlink("clang_rt.crtbegin.o", format!("{libdir}/{name}"))?;
      }
      for name in ["crtend.o", "crtendS.o"] {
         symlink("clang_rt.crtend.o", format!("{libdir}/{name}"))?;
      }
      Ok(())
   }

   /// runtime for -coverage and -fprofile-instr-generate, where clang looks
   /// for it per binfmt. On ELF it needs kernel headers (mmap flags), so
   /// stage1 only. *`ROCm`* is the separate `clang_rt.profile_rocm`,
   /// `WindowsMMap` the win32 mmap port.
   fn profile_runtime(stage: &Stage, src: &Path, out: &str) -> io::Result<()> {
      if stage.binfmt == "elf" && stage.linux_headers.is_none() {
         return Ok(());
      }
      let profile = format!("{}/compiler-rt/lib/profile", src.display());
      let coff = stage.binfmt == "coff";
      let sources = files(&format!("{profile}/*.{{c,cpp}}"))?;
      let kept = sources.into_iter().filter(|path| {
         let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
         !(name.contains("ROCm") || (!coff && name.starts_with("WindowsMMap")))
      });
      let target_profile = stage.profile()?;
      let mut flags = stage.target();
      flags.extend(Self::libc_includes(stage)?);
      flags.extend([
         "-O2".to_owned(),
         "-nostdinc++".to_owned(),
         "-w".to_owned(),
         format!("-I{}/compiler-rt/include", src.display()),
         format!("-I{profile}"),
      ]);
      flags.push("-DCOMPILER_RT_HAS_ATOMICS=1".to_owned());
      flags.extend(target_profile.pic().iter().map(|flag| (*flag).to_owned()));
      if target_profile.posix() {
         flags.extend([
            "-DCOMPILER_RT_HAS_FCNTL_LCK=1".to_owned(),
            "-DCOMPILER_RT_HAS_FLOCK=1".to_owned(),
            "-DCOMPILER_RT_HAS_UNAME=1".to_owned(),
         ]);
      }
      let lib = format!("{out}/{}", target_profile.runtime("profile"));
      let items: Vec<Item> = kept
         .map(|source| {
            let name = source
               .file_name()
               .map(|name| name.to_string_lossy().into_owned())
               .unwrap_or_default();
            Item {
               obj:   PathBuf::from(format!("{}/obj/profile/{name}.o", stage.build_top)),
               src:   source,
               flags: Vec::new(),
            }
         })
         .collect();
      let objs = compile(&flags, &items)?;
      archive(Path::new(&lib), &objs)
   }
}

impl Recipe for CompilerRt {
   fn name(&self) -> &'static str {
      "compiler-rt"
   }

   /// compiler-rt without cmake, the builtins from the per-target file list
   /// (pkgs/ll/llvm/builtins-*.txt, written by its update.nu) with the flags
   /// cmake would use, laid out as a clang resource dir. No
   /// -`DCOMPILER_RT_HAS_FLOAT16` on ppc, clang has no _Float16 there.
   fn run(&self, stage: &Stage) -> io::Result<()> {
      let out = stage.out();
      let src = stage.unpack("llvm", &["compiler-rt", "third-party/siphash"])?;
      let builtins = format!("{}/compiler-rt/lib/builtins", src.display());
      let obj = format!("{}/obj", stage.build_top);
      let pic = stage.profile()?.pic();
      let percpu: &[&str] = match stage.cpu.as_str() {
         "powerpc64le" => &[],
         "aarch64" => {
            &[
               "-DCOMPILER_RT_HAS_FLOAT16",
               "-DENABLE_BAREMETAL_AARCH64_FMV",
               "-DHAS_ASM_LSE",
            ]
         },
         _ => &["-DCOMPILER_RT_HAS_FLOAT16"],
      };
      let mut common = stage.target();
      common.extend(Self::libc_includes(stage)?);
      common.extend(pic.iter().map(|flag| (*flag).to_owned()));
      common.extend(percpu.iter().map(|flag| (*flag).to_owned()));
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
         .into_iter()
         .map(str::to_owned),
      );
      common.push(format!("-I{builtins}"));
      common.push(format!("-I{}/third-party/siphash/include", src.display()));

      let list = wanted(stage.list.as_deref(), "list")?;
      let items: Vec<Item> = read_list(Path::new(list))?
         .iter()
         .map(|file| Self::builtin_item(&builtins, &obj, file))
         .collect();
      say(&format!(
         "compiler-rt builtins {}: {} objects",
         stage.cpu,
         items.len()
      ));
      let lib = Self::builtins_lib(stage, out)?;
      if let Some(parent) = Path::new(&lib).parent() {
         create_dir_all(parent)?;
      }
      let objs = compile(&common, &items)?;
      archive(Path::new(&lib), &objs)?;

      let resource = capture("clang", &["--print-resource-dir"])?;
      copy_tree(
         &Path::new(resource.trim()).join("include"),
         Path::new(&format!("{out}/include")),
         "**/*",
      )?;
      if stage.binfmt == "elf" {
         Self::elf_extras(stage, &src, out, &common)?;
      }
      Self::profile_runtime(stage, &src, out)?;
      Ok(())
   }
}
