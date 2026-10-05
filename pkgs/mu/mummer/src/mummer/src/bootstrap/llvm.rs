use std::{
   fs::{
      canonicalize,
      copy,
      create_dir_all,
      remove_dir_all,
      remove_file,
   },
   io,
   os::unix::fs::symlink,
   path::{
      Path,
      PathBuf,
   },
};

use mummer::tree::parallel_with;

use crate::bootstrap::{
   dispatch::Recipe,
   stage::{
      Stage,
      archive,
      capture,
      cores,
      files,
      run,
      run_in,
      say,
      strs,
      tool,
      wanted,
   },
};

const TOOLCHAIN_TOOLS: &[&str] = &[
   "llvm-ar",
   "llvm-ranlib",
   "llvm-nm",
   "llvm-objcopy",
   "llvm-strip",
   "llvm-objdump",
   "llvm-readelf",
   "llvm-readobj",
   "llvm-size",
   "llvm-strings",
   "llvm-symbolizer",
   "llvm-cxxfilt",
   "llvm-cov",
   "llvm-profdata",
   "llvm-rc",
   "llvm-mt",
   "llvm-ml",
   "llvm-lib",
   "llvm-dlltool",
   "llvm-windres",
   "llvm-install-name-tool",
   "llvm-lipo",
   "llvm-otool",
   "dsymutil",
];

pub struct Llvm;

impl Llvm {
   fn compression_libraries(stage: &Stage) -> io::Result<PathBuf> {
      let prefix = PathBuf::from(&stage.build_top).join("complibs");

      for directory in ["include", "lib", "obj"] {
         create_dir_all(prefix.join(directory))?;
      }

      let compile = |flags: &[&str], sources: &[PathBuf]| -> io::Result<Vec<PathBuf>> {
         parallel_with(sources, cores(), |source| {
            let name = source
               .file_name()
               .ok_or_else(|| io::Error::other("compression source has no filename"))?;
            let object = prefix
               .join("obj")
               .join(format!("{}.o", name.to_string_lossy()));
            let mut args = vec!["-O2", "-fPIC"];
            args.extend_from_slice(flags);
            let source_text = source.to_string_lossy();
            let object_text = object.to_string_lossy();
            args.extend(["-c", &source_text, "-o", &object_text]);
            run("cc", &args)?;
            Ok(object)
         })
      };

      let zstd = wanted(stage.zstd.as_deref(), "zstd")?;
      let zstd_sources = files(&format!("{zstd}/lib/{{common,compress,decompress}}/*.c"))?;
      let zstd_objects = compile(
         &[
            "-DZSTD_DISABLE_ASM",
            "-DZSTD_LEGACY_SUPPORT=0",
            "-DZSTD_MULTITHREAD",
         ],
         &zstd_sources,
      )?;
      archive(&prefix.join("lib/libzstd.a"), &zstd_objects)?;

      for header in ["zstd.h", "zdict.h", "zstd_errors.h"] {
         copy(
            format!("{zstd}/lib/{header}"),
            prefix.join("include").join(header),
         )?;
      }

      let zlib = wanted(stage.zlib.as_deref(), "zlib")?;
      let zlib_objects = compile(
         &["-DHAVE_UNISTD_H", "-D_LARGEFILE64_SOURCE=1"],
         &files(&format!("{zlib}/*.c"))?,
      )?;
      archive(&prefix.join("lib/libz.a"), &zlib_objects)?;

      for header in ["zlib.h", "zconf.h"] {
         copy(
            format!("{zlib}/{header}"),
            prefix.join("include").join(header),
         )?;
      }

      Ok(prefix)
   }

   fn install_tools(stage: &Stage) -> io::Result<()> {
      let out = stage.out();

      for path in files(&format!(
         "{out}/bin/{{clang-*,diagtool,git-clang-format,hmaptool,*-arch}}"
      ))? {
         let name = path.file_name().unwrap_or_default().to_string_lossy();
         if name
            .strip_prefix("clang-")
            .is_some_and(|suffix| suffix.starts_with(|ch: char| ch.is_ascii_digit()))
         {
            continue;
         }

         remove_file(path)?;
      }

      for path in files(&format!("{out}/lib/lib{{clang,LTO,Remarks}}.so*"))? {
         remove_file(path)?;
      }

      for directory in ["share", "include/llvm-c", "include/clang-c"] {
         match remove_dir_all(Path::new(out).join(directory)) {
            Ok(()) => {},
            Err(error) if error.kind() == io::ErrorKind::NotFound => {},
            Err(error) => return Err(error),
         }
      }

      let mut strip_args = vec!["--strip-all".to_owned()];
      strip_args.extend(
         files(&format!("{out}/{{bin/*,lib/*.so*}}"))?
            .into_iter()
            .filter(|path| !path.is_symlink())
            .map(|path| path.to_string_lossy().into_owned()),
      );
      run("llvm-strip", &strs(&strip_args))?;

      for name in [
         "ar",
         "ranlib",
         "nm",
         "objcopy",
         "objdump",
         "strip",
         "readelf",
         "size",
         "strings",
         "c++filt",
         "addr2line",
      ] {
         let link = Path::new(out).join("bin").join(name);
         match remove_file(&link) {
            Ok(()) => {},
            Err(error) if error.kind() == io::ErrorKind::NotFound => {},
            Err(error) => return Err(error),
         }

         symlink(format!("llvm-{name}"), link)?;
      }

      let sysroot = wanted(stage.sysroot.as_deref(), "sysroot")?;

      for library in files(&format!("{sysroot}/lib/lib{{c++,c++abi,unwind}}.so.*"))? {
         let name = library.file_name().unwrap_or_default().to_string_lossy();
         if !name.rsplit_once('.').is_some_and(|(_, version)| {
            version.len() == 1 && version.bytes().all(|byte| byte.is_ascii_digit())
         }) {
            continue;
         }

         let filename = library
            .file_name()
            .ok_or_else(|| io::Error::other("runtime library has no filename"))?;
         copy(
            canonicalize(&library)?,
            Path::new(out).join("lib").join(filename),
         )?;
      }

      Ok(())
   }
}

impl Recipe for Llvm {
   fn name(&self) -> &'static str {
      "llvm"
   }

   fn run(&self, stage: &Stage) -> io::Result<()> {
      let source = stage.unpack("llvm-project", &[
         "llvm",
         "clang",
         "lld",
         "libc",
         "cmake",
         "third-party",
         "libunwind/include",
         "runtimes/cmake",
      ])?;

      for patch in &stage.patches {
         run_in(&source, "patch", &["-p1", "-i", patch])?;
      }

      let compression = Self::compression_libraries(stage)?;
      let build = PathBuf::from(&stage.build_top).join("build");
      let mut args: Vec<String> = [
         "-S",
         "llvm",
         "-B",
         &build.to_string_lossy(),
         "-G",
         "Unix Makefiles",
         "-DCMAKE_BUILD_TYPE=Release",
         "-DCMAKE_C_COMPILER=cc",
         "-DCMAKE_CXX_COMPILER=c++",
         "-DCMAKE_INSTALL_RPATH=$ORIGIN/../lib",
         "-DCMAKE_BUILD_WITH_INSTALL_RPATH=ON",
         "-DLLVM_ENABLE_PROJECTS=clang;lld",
         "-DLLVM_LINK_LLVM_DYLIB=ON",
         "-DLLVM_BUILD_LLVM_DYLIB=ON",
         "-DCLANG_LINK_CLANG_DYLIB=ON",
         "-DLLVM_ENABLE_ZLIB=FORCE_ON",
         "-DZLIB_USE_STATIC_LIBS=ON",
         "-DLLVM_ENABLE_ZSTD=FORCE_ON",
         "-DLLVM_USE_STATIC_ZSTD=ON",
         "-DLLVM_ENABLE_LIBXML2=OFF",
         "-DLLVM_ENABLE_TERMINFO=OFF",
         "-DLLVM_ENABLE_LIBEDIT=OFF",
         "-DLLVM_ENABLE_LIBPFM=OFF",
         "-DLLVM_INCLUDE_TESTS=OFF",
         "-DLLVM_INCLUDE_BENCHMARKS=OFF",
         "-DLLVM_INCLUDE_EXAMPLES=OFF",
         "-DLLVM_INCLUDE_DOCS=OFF",
         "-DCLANG_INCLUDE_TESTS=OFF",
         "-DCLANG_INCLUDE_DOCS=OFF",
         "-DCLANG_ENABLE_ARCMT=OFF",
         "-DCLANG_ENABLE_STATIC_ANALYZER=OFF",
         "-DCLANG_TOOL_CLANG_REPL_BUILD=OFF",
         "-DCLANG_ENABLE_HLSL=OFF",
         "-DCLANG_DEFAULT_LINKER=lld",
         "-DCLANG_DEFAULT_RTLIB=compiler-rt",
         "-DCLANG_DEFAULT_UNWINDLIB=libunwind",
         "-DCLANG_DEFAULT_CXX_STDLIB=libc++",
         "-DCLANG_DEFAULT_OBJCOPY=llvm-objcopy",
         "-DLLVM_INSTALL_TOOLCHAIN_ONLY=ON",
      ]
      .into_iter()
      .map(str::to_owned)
      .collect();
      args.extend([
         format!("-DCMAKE_MAKE_PROGRAM={}", tool("make")?.display()),
         format!("-DCMAKE_INSTALL_PREFIX={}", stage.out()),
         format!("-DCMAKE_AR={}", tool("llvm-ar")?.display()),
         format!("-DCMAKE_RANLIB={}", tool("llvm-ranlib")?.display()),
         format!("-DCMAKE_PREFIX_PATH={}", compression.display()),
         format!("-DPython3_EXECUTABLE={}", tool("python3")?.display()),
         format!(
            "-DLLVM_TARGETS_TO_BUILD={}",
            wanted(stage.targets.as_deref(), "targets")?
         ),
         format!("-DLLVM_HOST_TRIPLE={}", stage.clang_target),
         format!("-DLLVM_DEFAULT_TARGET_TRIPLE={}", stage.clang_target),
         format!("-DLLVM_TOOLCHAIN_TOOLS={}", TOOLCHAIN_TOOLS.join(";")),
      ]);
      run_in(&source, "cmake", &strs(&args))?;

      let shell_arg = format!("SHELL={}", tool("sh")?.display());
      run_in(&build, "make", &["-j", &cores().to_string(), &shell_arg])?;
      run_in(&build, "make", &["install/fast", &shell_arg])?;
      Self::install_tools(stage)?;
      let version = capture(&format!("{}/bin/clang", stage.out()), &["--version"])?;
      say(version.lines().next().unwrap_or_default());
      Ok(())
   }
}
