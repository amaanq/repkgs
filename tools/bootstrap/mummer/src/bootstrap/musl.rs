use std::{
   collections::BTreeSet,
   fs::{
      create_dir_all,
      read_dir,
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
   files::write_file,
};

use crate::bootstrap::{
   dispatch::Recipe,
   stage::{
      Item,
      LibcAttrs,
      Stage,
      archive,
      cc_facts,
      compile,
      copy_tree,
      cores,
      dirs,
      execute,
      files,
      say,
      write_rsp,
   },
};

fn kind(relative: &Path) -> &str {
   relative
      .components()
      .next()
      .and_then(|component| component.as_os_str().to_str())
      .unwrap_or_default()
}

pub struct Musl;

impl Musl {
   fn guard(kind: &str, name: &str, decl: &str) -> String {
      format!(
         "#if defined(__NEED_{kind}{name}) && !defined(__DEFINED_{kind}{name})\n{decl}\n#define \
          __DEFINED_{kind}{name}\n#endif\n"
      )
   }

   /// port of tools/mkalltypes.sed.
   fn mkalltypes(files: &[PathBuf]) -> io::Result<String> {
      let raw = files
         .iter()
         .map(read_to_string)
         .collect::<io::Result<String>>()?;
      let mut result = raw
         .lines()
         .map(|line| {
            if let Some(rest) = line.strip_prefix("TYPEDEF ")
               && let Some(without_semi) = rest.strip_suffix(';')
               && let Some((def, name)) = without_semi.rsplit_once(' ')
            {
               Self::guard("", name, &format!("typedef {def} {name};"))
            } else if let Some(rest) = line
               .strip_prefix("STRUCT * ")
               .or_else(|| line.strip_prefix("STRUCT "))
               && let Some(without_semi) = rest.strip_suffix(';')
               && let Some((name, def)) = without_semi.trim_start().split_once(' ')
            {
               Self::guard("struct_", name, &format!("struct {name} {def};"))
            } else if let Some(rest) = line
               .strip_prefix("UNION * ")
               .or_else(|| line.strip_prefix("UNION "))
               && let Some(without_semi) = rest.strip_suffix(';')
               && let Some((name, def)) = without_semi.trim_start().split_once(' ')
            {
               Self::guard("union_", name, &format!("union {name} {def};"))
            } else {
               line.to_owned()
            }
         })
         .collect::<Vec<_>>()
         .join("\n");
      result.push('\n');
      Ok(result)
   }

   fn headers(src: &Path, inc: &Path, arch: &str) -> io::Result<()> {
      copy_tree(&src.join("include"), inc, "**/*.h")?;
      copy_tree(&src.join("arch/generic/bits"), &inc.join("bits"), "*.h")?;
      copy_tree(
         &src.join(format!("arch/{arch}/bits")),
         &inc.join("bits"),
         "*.h",
      )?;
      let alltypes = Self::mkalltypes(&[
         src.join(format!("arch/{arch}/bits/alltypes.h.in")),
         src.join("include/alltypes.h.in"),
      ])?;
      write_file(&inc.join("bits/alltypes.h"), alltypes.as_bytes(), None)?;
      let mut syscalls = read_to_string(src.join(format!("arch/{arch}/bits/syscall.h.in")))?;
      let renamed = syscalls
         .lines()
         .filter(|line| line.contains("__NR_"))
         .map(|line| line.replace("__NR_", "SYS_"))
         .collect::<Vec<_>>();
      syscalls.push_str(&renamed.join("\n"));
      syscalls.push('\n');
      write_file(&inc.join("bits/syscall.h"), syscalls.as_bytes(), None)
   }

   fn stem_path(path: &Path) -> PathBuf {
      let stem = path.file_stem().unwrap_or_default();
      path
         .parent()
         .map_or_else(|| PathBuf::from(stem), |parent| parent.join(stem))
   }

   /// source set as the Makefile computes it, where arch files replace
   /// generic ones with the same stem.
   fn sources(src: &Path, arch: &str) -> io::Result<Vec<PathBuf>> {
      let mut directories = dirs(&format!("{}/src/*", src.display()))?;
      directories.extend([
         src.join("src/malloc/mallocng"),
         src.join("crt"),
         src.join("ldso"),
      ]);
      let mut generic = Vec::new();
      let mut specific = Vec::new();
      for directory in &directories {
         let shown = directory.display();
         generic.extend(files(&format!("{shown}/*.c"))?);
         specific.extend(files(&format!("{shown}/{arch}/*.{{c,s,S}}"))?);
      }
      let overridden = specific
         .iter()
         .filter_map(|file| {
            Some(
               file
                  .parent()?
                  .parent()?
                  .join(file.file_stem().unwrap_or_default()),
            )
         })
         .collect::<BTreeSet<_>>();
      let mut sources = generic
         .into_iter()
         .filter(|file| !overridden.contains(&Self::stem_path(file)))
         .chain(specific)
         .collect::<Vec<_>>();
      sources.sort_by_key(|path| path.to_string_lossy().into_owned());
      Ok(sources)
   }

   /// per-file flag classes from the Makefile, `OPTIMIZE_GLOBS`, MEMOPS, NOSSP
   /// and CRT.
   fn file_flags(relative: &Path) -> Vec<String> {
      let kind = kind(relative);
      let subdir = relative
         .components()
         .nth(1)
         .and_then(|component| component.as_os_str().to_str());
      let stem = relative
         .file_stem()
         .and_then(|part| part.to_str())
         .unwrap_or_default();
      let mut flags = Vec::new();
      if matches!(subdir, Some("internal" | "malloc" | "string")) {
         flags.push("-O3".to_owned());
      }
      if matches!(stem, "memcpy" | "memmove" | "memcmp" | "memset") {
         flags.push("-fno-builtin".to_owned());
      }
      if kind == "crt" {
         flags.push("-DCRT".to_owned());
      }
      if kind != "src"
         || matches!(
            stem,
            "__libc_start_main"
               | "__init_tls"
               | "__stack_chk_fail"
               | "__set_thread_area"
               | "memset"
               | "memcpy"
         )
      {
         flags.push("-fno-stack-protector".to_owned());
      }
      if matches!(stem, "Scrt1" | "rcrt1") {
         flags.push("-fPIC".to_owned());
      }
      flags
   }

   /// what musl configure picks for clang, -nostdinc because musl brings its
   /// own stdarg and stddef.
   fn common_flags(stage: &Stage, src: &Path, obj: &Path, arch: &str) -> Vec<String> {
      let source = src.display();
      let mut common = stage.target();
      common.extend(
         [
            "-std=c99",
            "-nostdinc",
            "-ffreestanding",
            "-fexcess-precision=standard",
            "-frounding-math",
            "-fno-strict-aliasing",
            "-Wa,--noexecstack",
            "-D_XOPEN_SOURCE=700",
         ]
         .map(str::to_owned),
      );
      common.extend([
         format!("-I{source}/arch/{arch}"),
         format!("-I{source}/arch/generic"),
         format!("-I{}/src/internal", obj.display()),
         format!("-I{source}/src/include"),
         format!("-I{source}/src/internal"),
         format!("-I{}/include", stage.out.display()),
      ]);
      common.extend(
         [
            "-O2",
            "-pipe",
            "-fomit-frame-pointer",
            "-fno-unwind-tables",
            "-fno-asynchronous-unwind-tables",
            "-ffunction-sections",
            "-fdata-sections",
            "-fPIC",
            "-w",
            "-Qunused-arguments",
            "-Werror=implicit-function-declaration",
            "-Werror=implicit-int",
            "-Werror=pointer-sign",
            "-Werror=pointer-arith",
            "-Werror=int-conversion",
            "-Werror=incompatible-pointer-types",
         ]
         .map(str::to_owned),
      );
      common
   }

   fn items(src: &Path, obj: &Path, out: &Path, arch: &str) -> io::Result<[Vec<Item>; 3]> {
      let mut libc = Vec::new();
      let mut ldso = Vec::new();
      let mut crt = Vec::new();
      for source in Self::sources(src, arch)? {
         let relative = source
            .strip_prefix(src)
            .map_err(io::Error::other)?
            .to_path_buf();
         let flags = Self::file_flags(&relative);
         let (list, object) = match kind(&relative) {
            "src" => (&mut libc, obj.join(format!("{}.o", relative.display()))),
            "ldso" => (&mut ldso, obj.join(format!("{}.o", relative.display()))),
            "crt" => {
               let stem = source.file_stem().unwrap_or_default().to_string_lossy();
               (&mut crt, out.join(format!("lib/{stem}.o")))
            },
            _ => continue,
         };
         list.push(Item {
            src: source,
            obj: object,
            flags,
         });
      }
      Ok([libc, ldso, crt])
   }

   fn link_libc(
      stage: &Stage,
      compiler_rt: &Path,
      src: &Path,
      rsp: &Path,
      arch: &str,
   ) -> io::Result<()> {
      let lib = stage.out.join("lib");
      execute(
         Command::new("clang")
            .args(stage.target())
            .args([
               "-fuse-ld=lld",
               "-nostdlib",
               "-shared",
               "-Wl,-e,_dlstart",
               "-Wl,--sort-section,alignment",
               "-Wl,--sort-common",
               "-Wl,--gc-sections",
               "-Wl,--hash-style=both",
               "-Wl,--no-undefined",
               "-Wl,--exclude-libs=ALL",
            ])
            .arg(format!("-Wl,--dynamic-list={}/dynamic.list", src.display()))
            .arg("-o")
            .arg(lib.join("libc.so"))
            .arg(format!("@{}", rsp.display()))
            .arg(compiler_rt.join(format!("lib/{}/libclang_rt.builtins.a", stage.clang_target))),
      )?;
      symlink("libc.so", lib.join(format!("ld-musl-{arch}.so.1")))
   }
}

impl Recipe for Musl {
   fn name(&self) -> &'static str {
      "musl"
   }

   /// musl built directly with clang because its configure and Makefile need
   /// sh and make which do not exist yet. Mirrors musl Makefile with generated
   /// headers, arch overrides, and per-file flag classes. With `headers_only`
   /// it stops after headers because compiler-rt needs them before libc.so
   /// can link.
   fn run(&self, stage: &Stage) -> io::Result<()> {
      let attrs = load::<LibcAttrs>()?;
      let out = &stage.out;
      let arch = stage.cpu.name();
      let src = stage.unpack("musl", &[])?;
      let obj = stage.build_top.join("obj");
      Self::headers(&src, &out.join("include"), arch)?;
      cc_facts(out, &[("include-dirs", &["include"])])?;
      if attrs.headers_only() {
         return Ok(());
      }
      create_dir_all(obj.join("src/internal"))?;
      let version = read_to_string(src.join("VERSION"))?;
      write_file(
         &obj.join("src/internal/version.h"),
         format!("#define VERSION \"{}\"\n", version.trim()).as_bytes(),
         None,
      )?;

      let common = Self::common_flags(stage, &src, &obj, arch);
      let [libc_items, ldso_items, crt_items] = Self::items(&src, &obj, out, arch)?;
      say(&format!(
         "musl: {} libc sources, {} jobs",
         libc_items.len(),
         cores()
      ));
      create_dir_all(out.join("lib"))?;

      // everything is -fPIC, so one object set serves libc.a and libc.so,
      // musl Makefile builds .o and .lo separately only to allow a non-PIC
      // static lib
      let objs = compile("clang", &common, &libc_items)?;
      let ldso = compile("clang", &common, &ldso_items)?;
      compile("clang", &common, &crt_items)?;

      archive(&out.join("lib/libc.a"), &objs)?;
      // empty compat libs
      for name in [
         "m", "rt", "pthread", "crypt", "util", "xnet", "resolv", "dl",
      ] {
         execute(
            Command::new("llvm-ar")
               .arg("rcD")
               .arg(out.join(format!("lib/lib{name}.a"))),
         )?;
      }

      let rsp = obj.join("libc.so.rsp");
      write_rsp(&rsp, &[objs, ldso].concat())?;
      Self::link_libc(stage, attrs.required_compiler_rt()?, &src, &rsp, arch)?;
      say(&format!(
         "musl: {} files in lib/",
         read_dir(out.join("lib"))?
            .collect::<io::Result<Vec<_>>>()?
            .len()
      ));
      Ok(())
   }
}
