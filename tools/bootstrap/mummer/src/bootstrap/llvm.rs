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
   process::Command,
};

use mummer::{
   files::utf8,
   attrs::load,
   files::remove_if_exists,
};
use serde::Deserialize;

use crate::bootstrap::{
   dispatch::Recipe,
   stage::{
      Item,
      Stage,
      archive,
      capture,
      compile,
      execute,
      files,
      make_parallel,
      make_target,
      say,
      tool,
   },
};

#[derive(Deserialize)]
struct Attrs {
   zstd:    PathBuf,
   zlib:    PathBuf,
   targets: String,
}

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
   /// Static zstd and zlib for LLVM to link, built from source with the
   /// stage's cc.
   fn compression_libraries(stage: &Stage, attrs: &Attrs) -> io::Result<PathBuf> {
      let prefix = stage.build_top.join("complibs");
      for directory in ["include", "lib", "obj"] {
         create_dir_all(prefix.join(directory))?;
      }
      let build = |flags: &[&str], sources: Vec<PathBuf>| -> io::Result<Vec<PathBuf>> {
         let items = sources
            .into_iter()
            .map(|source| {
               let name = source
                  .file_name()
                  .ok_or_else(|| io::Error::other("compression source has no filename"))?;
               Ok(Item {
                  obj:   prefix
                     .join("obj")
                     .join({
                        let mut object = name.to_owned();
                        object.push(".o");
                        object
                     }),
                  src:   source,
                  flags: Vec::new(),
               })
            })
            .collect::<io::Result<Vec<_>>>()?;
         let common = ["-O2", "-fPIC"]
            .iter()
            .chain(flags)
            .copied()
            .map(str::to_owned)
            .collect::<Vec<_>>();
         compile("cc", &common, &items)
      };

      let zstd = &attrs.zstd;
      let zstd_objects = build(
         &[
            "-DZSTD_DISABLE_ASM",
            "-DZSTD_LEGACY_SUPPORT=0",
            "-DZSTD_MULTITHREAD",
         ],
         files(&format!(
            "{}/lib/{{common,compress,decompress}}/*.c",
            zstd.display()
         ))?,
      )?;
      archive(&prefix.join("lib/libzstd.a"), &zstd_objects)?;
      for header in ["zstd.h", "zdict.h", "zstd_errors.h"] {
         copy(
            zstd.join("lib").join(header),
            prefix.join("include").join(header),
         )?;
      }

      let zlib = &attrs.zlib;
      let zlib_objects = build(
         &["-DHAVE_UNISTD_H", "-D_LARGEFILE64_SOURCE=1"],
         files(&format!("{}/*.c", zlib.display()))?,
      )?;
      archive(&prefix.join("lib/libz.a"), &zlib_objects)?;
      for header in ["zlib.h", "zconf.h"] {
         copy(zlib.join(header), prefix.join("include").join(header))?;
      }
      Ok(prefix)
   }

   fn install_tools(stage: &Stage) -> io::Result<()> {
      let out = &stage.out;
      let shown = out.display();
      for path in files(&format!(
         "{shown}/bin/{{clang-*,diagtool,git-clang-format,hmaptool,*-arch}}"
      ))? {
         let name = utf8(Path::new(path.file_name().unwrap_or_default()))?;
         if name
            .strip_prefix("clang-")
            .is_some_and(|suffix| suffix.starts_with(|ch: char| ch.is_ascii_digit()))
         {
            continue;
         }
         remove_file(path)?;
      }
      for path in files(&format!("{shown}/lib/lib{{clang,LTO,Remarks}}.so*"))? {
         remove_file(path)?;
      }
      for directory in ["share", "include/llvm-c", "include/clang-c"] {
         match remove_dir_all(out.join(directory)) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
            Ok(()) | Err(_) => {},
         }
      }
      execute(
         Command::new("llvm-strip").arg("--strip-all").args(
            files(&format!("{shown}/{{bin/*,lib/*.so*}}"))?
               .into_iter()
               .filter(|path| !path.is_symlink()),
         ),
      )?;
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
         let link = out.join("bin").join(name);
         remove_if_exists(&link)?;
         symlink(format!("llvm-{name}"), link)?;
      }
      for library in files(&format!(
         "{}/lib/lib{{c++,c++abi,unwind}}.so.*",
         stage.sysroot()?.display()
      ))? {
         let Some(filename) = library.file_name() else {
            continue;
         };
         let single_version =
            utf8(Path::new(filename))?
               .rsplit_once('.')
               .is_some_and(|(_, version)| {
                  version.len() == 1 && version.bytes().all(|byte| byte.is_ascii_digit())
               });
         if single_version {
            copy(canonicalize(&library)?, out.join("lib").join(filename))?;
         }
      }
      Ok(())
   }

   fn configure(stage: &Stage, attrs: &Attrs, source: &Path, build: &Path) -> io::Result<()> {
      let compression = Self::compression_libraries(stage, attrs)?;
      execute(
         Command::new("cmake")
            .args(["-S", "llvm", "-B"])
            .arg(build)
            .args([
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
            ])
            .arg(format!("-DCMAKE_MAKE_PROGRAM={}", tool("make")?.display()))
            .arg(format!("-DCMAKE_INSTALL_PREFIX={}", stage.out.display()))
            .arg(format!("-DCMAKE_AR={}", tool("llvm-ar")?.display()))
            .arg(format!("-DCMAKE_RANLIB={}", tool("llvm-ranlib")?.display()))
            .arg(format!("-DCMAKE_PREFIX_PATH={}", compression.display()))
            .arg(format!(
               "-DPython3_EXECUTABLE={}",
               tool("python3")?.display()
            ))
            .arg(format!("-DLLVM_TARGETS_TO_BUILD={}", attrs.targets))
            .arg(format!("-DLLVM_HOST_TRIPLE={}", stage.clang_target))
            .arg(format!(
               "-DLLVM_DEFAULT_TARGET_TRIPLE={}",
               stage.clang_target
            ))
            .arg(format!(
               "-DLLVM_TOOLCHAIN_TOOLS={}",
               TOOLCHAIN_TOOLS.join(";")
            ))
            .current_dir(source),
      )
   }
}

impl Recipe for Llvm {
   fn name(&self) -> &'static str {
      "llvm"
   }

   fn run(&self, stage: &Stage) -> io::Result<()> {
      let attrs = load::<Attrs>()?;
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
      let build = stage.build_top.join("build");
      Self::configure(stage, &attrs, &source, &build)?;
      execute(&mut make_parallel(&build)?)?;
      execute(&mut make_target(&build, "install/fast")?)?;
      Self::install_tools(stage)?;
      let version = capture(Command::new(stage.out.join("bin/clang")).arg("--version"))?;
      say(version.lines().next().unwrap_or_default());
      Ok(())
   }
}
