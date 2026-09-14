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
};

use jig::files::write_file;

use crate::bootstrap::{
   dispatch::Recipe,
   stage::{
      Item,
      Stage,
      archive,
      cc_facts,
      compile,
      copy_tree,
      cores,
      dirs,
      files,
      run,
      say,
      strs,
      wanted,
   },
};

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
      let mut raw = String::new();
      for file in files {
         raw.push_str(&read_to_string(file)?);
      }
      let mut line_list = Vec::new();
      for line in raw.lines() {
         if let Some(rest) = line.strip_prefix("TYPEDEF ")
            && let Some(without_semi) = rest.strip_suffix(';')
            && let Some((def, name)) = without_semi.rsplit_once(' ')
         {
            line_list.push(Self::guard("", name, &format!("typedef {def} {name};")));
         } else if let Some(rest) = line
            .strip_prefix("STRUCT * ")
            .or_else(|| line.strip_prefix("STRUCT "))
            && let Some(without_semi) = rest.strip_suffix(';')
            && let Some((name, def)) = without_semi.trim_start().split_once(' ')
         {
            line_list.push(Self::guard(
               "struct_",
               name,
               &format!("struct {name} {def};"),
            ));
         } else if let Some(rest) = line
            .strip_prefix("UNION * ")
            .or_else(|| line.strip_prefix("UNION "))
            && let Some(without_semi) = rest.strip_suffix(';')
            && let Some((name, def)) = without_semi.trim_start().split_once(' ')
         {
            line_list.push(Self::guard("union_", name, &format!("union {name} {def};")));
         } else {
            line_list.push(line.to_owned());
         }
      }
      let mut result = line_list.join("\n");
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
      let syscall_in = src.join(format!("arch/{arch}/bits/syscall.h.in"));
      let syscall_content = read_to_string(syscall_in)?;
      let mut syscall_lines = Vec::new();
      for line in syscall_content.lines() {
         if line.contains("__NR_") {
            syscall_lines.push(line.replace("__NR_", "SYS_"));
         }
      }
      let mut combined = syscall_content;
      combined.push_str(&syscall_lines.join("\n"));
      combined.push('\n');
      write_file(&inc.join("bits/syscall.h"), combined.as_bytes(), None)?;
      Ok(())
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
      let src_text = src.to_string_lossy();
      let mut dir_list = dirs(&format!("{src_text}/src/*"))?;
      dir_list.push(PathBuf::from(format!("{src_text}/src/malloc/mallocng")));
      dir_list.push(PathBuf::from(format!("{src_text}/crt")));
      dir_list.push(PathBuf::from(format!("{src_text}/ldso")));
      let mut generic = Vec::new();
      let mut specific = Vec::new();
      for dir in &dir_list {
         let dir_text = dir.to_string_lossy();
         generic.extend(files(&format!("{dir_text}/*.c"))?);
         specific.extend(files(&format!("{dir_text}/{arch}/*.{{c,s,S}}"))?);
      }
      let mut overridden = BTreeSet::new();
      for file in &specific {
         if let Some(parent) = file.parent().and_then(Path::parent) {
            let stem = file.file_stem().unwrap_or_default();
            overridden.insert(parent.join(stem));
         }
      }
      let mut source_list = Vec::new();
      for file in generic {
         if !overridden.contains(&Self::stem_path(&file)) {
            source_list.push(file);
         }
      }
      source_list.extend(specific);
      source_list.sort_by_key(|path| path.to_string_lossy().into_owned());
      Ok(source_list)
   }

   /// per-file flag classes from the Makefile, `OPTIMIZE_GLOBS`, MEMOPS, NOSSP
   /// and CRT.
   fn file_flags(relative: &Path) -> Vec<String> {
      let mut components = relative.components();
      let kind = components
         .next()
         .and_then(|comp| comp.as_os_str().to_str())
         .unwrap_or_default();
      let subdir = components.next().and_then(|comp| comp.as_os_str().to_str());
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

   fn common_flags(stage: &Stage, src: &Path, obj: &Path, arch: &str) -> Vec<String> {
      let src_text = src.to_string_lossy();
      let obj_text = obj.to_string_lossy();
      let out = stage.out();
      let mut common = stage.target();
      common.extend([
         "-std=c99".to_owned(),
         "-nostdinc".to_owned(),
         "-ffreestanding".to_owned(),
         "-fexcess-precision=standard".to_owned(),
         "-frounding-math".to_owned(),
         "-fno-strict-aliasing".to_owned(),
         "-Wa,--noexecstack".to_owned(),
         "-D_XOPEN_SOURCE=700".to_owned(),
         format!("-I{src_text}/arch/{arch}"),
         format!("-I{src_text}/arch/generic"),
         format!("-I{obj_text}/src/internal"),
         format!("-I{src_text}/src/include"),
         format!("-I{src_text}/src/internal"),
         format!("-I{out}/include"),
         "-O2".to_owned(),
         "-pipe".to_owned(),
         "-fomit-frame-pointer".to_owned(),
         "-fno-unwind-tables".to_owned(),
         "-fno-asynchronous-unwind-tables".to_owned(),
         "-ffunction-sections".to_owned(),
         "-fdata-sections".to_owned(),
         "-fPIC".to_owned(),
         "-w".to_owned(),
         "-Qunused-arguments".to_owned(),
         "-Werror=implicit-function-declaration".to_owned(),
         "-Werror=implicit-int".to_owned(),
         "-Werror=pointer-sign".to_owned(),
         "-Werror=pointer-arith".to_owned(),
         "-Werror=int-conversion".to_owned(),
         "-Werror=incompatible-pointer-types".to_owned(),
      ]);
      common
   }

   fn link_libc(stage: &Stage, src: &Path, rsp: &Path, arch: &str) -> io::Result<()> {
      let out = stage.out();
      let compiler_rt = wanted(stage.compiler_rt.as_deref(), "compiler-rt")?;
      let mut link_args = stage.target();
      link_args.extend([
         "-fuse-ld=lld".to_owned(),
         "-nostdlib".to_owned(),
         "-shared".to_owned(),
         "-Wl,-e,_dlstart".to_owned(),
         "-Wl,--sort-section,alignment".to_owned(),
         "-Wl,--sort-common".to_owned(),
         "-Wl,--gc-sections".to_owned(),
         "-Wl,--hash-style=both".to_owned(),
         "-Wl,--no-undefined".to_owned(),
         "-Wl,--exclude-libs=ALL".to_owned(),
         format!("-Wl,--dynamic-list={}/dynamic.list", src.to_string_lossy()),
         "-o".to_owned(),
         format!("{out}/lib/libc.so"),
         format!("@{}", rsp.to_string_lossy()),
         format!(
            "{compiler_rt}/lib/{}/libclang_rt.builtins.a",
            stage.clang_target
         ),
      ]);
      run("clang", &strs(&link_args))?;
      symlink("libc.so", format!("{out}/lib/ld-musl-{arch}.so.1"))
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
      let out = stage.out();
      let arch = &stage.cpu;
      let src = stage.unpack("musl", &[])?;
      let obj = PathBuf::from(&stage.build_top).join("obj");
      Self::headers(&src, Path::new(&format!("{out}/include")), arch)?;
      cc_facts(out, &[("include-dirs", &["include"])])?;
      if stage.headers_only() {
         return Ok(());
      }
      create_dir_all(obj.join("src/internal"))?;
      let version = read_to_string(src.join("VERSION"))?;
      write_file(
         &obj.join("src/internal/version.h"),
         format!("#define VERSION \"{}\"\n", version.trim()).as_bytes(),
         None,
      )?;

      // what musl configure picks for clang, -nostdinc because musl brings its
      // own stdarg and stddef
      let common = Self::common_flags(stage, &src, &obj, arch);
      let all_sources = Self::sources(&src, arch)?;
      let mut src_items = Vec::new();
      let mut ldso_items = Vec::new();
      let mut crt_items = Vec::new();
      for source in all_sources {
         let relative = source.strip_prefix(&src).unwrap_or(&source).to_path_buf();
         let kind = relative
            .components()
            .next()
            .and_then(|comp| comp.as_os_str().to_str())
            .unwrap_or_default()
            .to_owned();
         let flags = Self::file_flags(&relative);
         let object = match kind.as_str() {
            "src" | "ldso" => obj.join(format!("{}.o", relative.display())),
            "crt" => {
               let stem = source.file_stem().unwrap_or_default().to_string_lossy();
               PathBuf::from(format!("{out}/lib/{stem}.o"))
            },
            _ => continue,
         };
         let item = Item {
            src: source,
            obj: object,
            flags,
         };
         match kind.as_str() {
            "src" => src_items.push(item),
            "ldso" => ldso_items.push(item),
            _ => crt_items.push(item),
         }
      }

      say(&format!(
         "musl: {} libc sources, {} jobs",
         src_items.len(),
         cores()
      ));
      create_dir_all(format!("{out}/lib"))?;

      // everything is -fPIC, so one object set serves libc.a and libc.so,
      // musl Makefile builds .o and .lo separately only to allow a non-PIC
      // static lib
      let objs = compile(&common, &src_items)?;
      let ldso = compile(&common, &ldso_items)?;
      compile(&common, &crt_items)?;

      archive(Path::new(&format!("{out}/lib/libc.a")), &objs)?;
      // empty compat libs
      for name in [
         "m", "rt", "pthread", "crypt", "util", "xnet", "resolv", "dl",
      ] {
         run("llvm-ar", &["rcD", &format!("{out}/lib/lib{name}.a")])?;
      }

      let rsp = obj.join("libc.so.rsp");
      let rsp_lines: Vec<String> = objs
         .iter()
         .chain(&ldso)
         .map(|path| path.to_string_lossy().into_owned())
         .collect();
      let mut rsp_content = rsp_lines.join("\n");
      rsp_content.push('\n');
      write_file(&rsp, rsp_content.as_bytes(), None)?;

      Self::link_libc(stage, &src, &rsp, arch)?;
      say(&format!(
         "musl: {} files in lib/",
         read_dir(format!("{out}/lib"))?.count()
      ));
      Ok(())
   }
}
