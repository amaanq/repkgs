use std::{
   collections::BTreeMap,
   env::set_current_dir,
   ffi::OsStr,
   fs::{
      copy,
      create_dir_all,
      remove_file,
   },
   io,
   os::unix::fs::symlink,
   path::{
      Path,
      PathBuf,
   },
};

use mummer::files::write_file;

use crate::bootstrap::{
   dispatch::Recipe,
   stage::{
      Item,
      Stage,
      archive,
      compile,
      copy_tree,
      files,
      run,
      say,
      strs,
   },
};

struct Lib {
   name:         &'static str,
   dir:          &'static str,
   glob:         &'static str,
   glob_windows: Option<&'static str>,
   skip:         &'static [&'static str],
   skip_windows: &'static [&'static str],
   extra:        &'static [&'static str],
   std:          &'static str,
   so:           Option<&'static [&'static str]>,
   dll:          bool,
   flags:        &'static [&'static str],
   posix_flags:  &'static [&'static str],
}

struct Built {
   objects: Vec<PathBuf>,
   dll:     Vec<PathBuf>,
}

/// sources are each project's src/ minus the files cmake would leave out for
/// a Linux, ELF, pthread, compiler-rt configuration. glibc dlopens
/// libunwind.so.1 and dlsyms __`gcc_personality_v0` too
/// (glibc-unwind-origin.patch). cmake would also pass
/// -`DHAVE___CXA_THREAD_ATEXIT_IMPL`, its probe is fooled by
/// `COMPILER_WORKS=ON`, musl lacks that symbol and libc++abi's own fallback is
/// fine on glibc too. `cxa_noexception.cpp` is the -fno-exceptions alternative
/// to `cxa_exception/cxa_personality`. new.cpp lives in libc++abi
/// (`stdlib_new_delete.cpp`) when built against it, int128 builtins come from
/// compiler-rt, libdispatch is the Apple PSTL backend.
const LIBS: [Lib; 4] = [
   Lib {
      name:         "unwind",
      dir:          "libunwind/src",
      glob:         "*.{cpp,c,S}",
      glob_windows: None,
      skip:         &["Unwind_AIXExtras.cpp"],
      skip_windows: &[],
      extra:        &["compiler-rt/lib/builtins/gcc_personality_v0.c"],
      std:          "c++17",
      so:           Some(&["-lc"]),
      dll:          true,
      flags:        &[
         "-D_LIBUNWIND_IS_NATIVE_ONLY",
         "-fno-exceptions",
         "-fno-rtti",
         "-fstrict-aliasing",
      ],
      posix_flags:  &["-D_LIBUNWIND_LINK_DL_LIB", "-D_LIBUNWIND_LINK_PTHREAD_LIB"],
   },
   Lib {
      name:         "c++abi",
      dir:          "libcxxabi/src",
      glob:         "*.cpp",
      glob_windows: None,
      skip:         &["cxa_noexception.cpp"],
      skip_windows: &[],
      extra:        &[],
      std:          "c++26",
      so:           Some(&["-lunwind", "-lc"]),
      dll:          false,
      flags:        &[
         "-D_LIBCPP_BUILDING_LIBRARY",
         "-D_LIBCXXABI_BUILDING_LIBRARY",
         "-DLIBCXX_BUILDING_LIBCXXABI",
         "-fstrict-aliasing",
         "-fsized-deallocation",
         "-Ilibcxx/src",
      ],
      posix_flags:  &["-D_LIBCXXABI_LINK_PTHREAD_LIB"],
   },
   Lib {
      name:         "c++",
      dir:          "libcxx/src",
      glob:         "{*,filesystem/*,ryu/*,pstl/*}.cpp",
      glob_windows: Some("support/win32/*.cpp"),
      skip:         &[
         "new.cpp",
         "filesystem/int128_builtins.cpp",
         "pstl/libdispatch.cpp",
      ],
      skip_windows: &[],
      extra:        &[],
      std:          "c++26",
      so:           Some(&["-lc++abi", "-lunwind", "-lc"]),
      dll:          true,
      flags:        &[
         "-D_LIBCPP_BUILDING_LIBRARY",
         "-D_LIBCPP_LINK_RT_LIB",
         "-D_LIBCPP_REMOVE_TRANSITIVE_INCLUDES",
         "-DLIBCXX_BUILDING_LIBCXXABI",
         "-DLIBC_NAMESPACE=__llvm_libc_common_utils",
         "-fvisibility=hidden",
         "-faligned-allocation",
         "-fsized-deallocation",
         "-Ilibcxx/src",
         "-Ilibc",
      ],
      posix_flags:  &["-D_LIBCPP_LINK_PTHREAD_LIB"],
   },
   Lib {
      name:         "c++experimental",
      dir:          "libcxx/src/experimental",
      glob:         "*.cpp",
      glob_windows: None,
      skip:         &[],
      skip_windows: &[
         "chrono_exception.cpp",
         "time_zone.cpp",
         "tzdb.cpp",
         "tzdb_list.cpp",
      ],
      extra:        &[],
      std:          "c++26",
      so:           None,
      dll:          false,
      flags:        &[
         "-D_LIBCPP_BUILDING_LIBRARY",
         "-D_LIBCPP_LINK_RT_LIB",
         "-D_LIBCPP_REMOVE_TRANSITIVE_INCLUDES",
         "-DLIBCXX_BUILDING_LIBCXXABI",
         "-D_LIBCPP_ENABLE_EXPERIMENTAL",
         "-fvisibility=hidden",
         "-faligned-allocation",
         "-fsized-deallocation",
      ],
      posix_flags:  &["-D_LIBCPP_LINK_PTHREAD_LIB"],
   },
];

pub struct Runtimes;

impl Runtimes {
   /// libcxx/include/__`config_site.in` as cmake fills it for stable ABI v1
   /// namespace __1, pthreads, filesystem, localization, unicode, wide chars
   /// and tzdb on, the `std::thread` PSTL backend, hardening "fast" (4) with
   /// the hardening-dependent assertion semantic (2), as
   /// libcxx/CMakeLists.txt encodes `LIBCXX_HARDENING_MODE` and
   /// `LIBCXX_ASSERTION_SEMANTIC`.
   fn config_site(stage: &Stage) -> String {
      let musl = u8::from(stage.libc == "musl");
      let tzdb = u8::from(stage.os != "windows");
      format!(
         "#ifndef _LIBCPP___CONFIG_SITE
#define _LIBCPP___CONFIG_SITE
#define _LIBCPP_ABI_VERSION 1
#define _LIBCPP_ABI_NAMESPACE __1
#define _LIBCPP_ABI_FORCE_ITANIUM 0
#define _LIBCPP_ABI_FORCE_MICROSOFT 0
#define _LIBCPP_HAS_THREADS 1
#define _LIBCPP_HAS_MONOTONIC_CLOCK 1
#define _LIBCPP_HAS_MUSL_LIBC {musl}
#define _LIBCPP_HAS_THREAD_API_PTHREAD 0
#define _LIBCPP_HAS_THREAD_API_EXTERNAL 0
#define _LIBCPP_HAS_THREAD_API_WIN32 0
#define _LIBCPP_HAS_THREAD_API_C11 0
#define _LIBCPP_HAS_VENDOR_AVAILABILITY_ANNOTATIONS 0
#define _LIBCPP_HAS_FILESYSTEM 1
#define _LIBCPP_HAS_RANDOM_DEVICE 1
#define _LIBCPP_HAS_LOCALIZATION 1
#define _LIBCPP_HAS_UNICODE 1
#define _LIBCPP_HAS_WIDE_CHARACTERS 1
#define _LIBCPP_HAS_TIME_ZONE_DATABASE {tzdb}
#define _LIBCPP_INSTRUMENTED_WITH_ASAN 0
#define _LIBCPP_PSTL_BACKEND_STD_THREAD
#define _LIBCPP_HARDENING_MODE_DEFAULT 4
#define _LIBCPP_ASSERTION_SEMANTIC_DEFAULT 2
#define _LIBCPP_LIBC_PICOLIBC 0
#define _LIBCPP_LIBC_NEWLIB 0
#define _LIBCPP_LIBC_LLVM_LIBC 0
#endif
"
      )
   }

   /// include/ as `ninja install` would lay it out, c++/v1 plus the two
   /// generated headers, cxxabi.h, unwind.h and co.
   fn install_headers(stage: &Stage, src: &Path, inc: &str) -> io::Result<()> {
      let v1 = format!("{inc}/c++/v1");
      create_dir_all(&v1)?;
      run("cp", &[
         "-r",
         &format!("{}/libcxx/include/.", src.display()),
         &v1,
      ])?;
      for junk in ["CMakeLists.txt", "__config_site.in", "module.modulemap.in"] {
         match remove_file(format!("{v1}/{junk}")) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
            _ => {},
         }
      }
      write_file(
         Path::new(&format!("{v1}/__config_site")),
         Self::config_site(stage).as_bytes(),
         None,
      )?;
      copy(
         src.join("libcxx/vendor/llvm/default_assertion_handler.in"),
         format!("{v1}/__assertion_handler"),
      )?;
      for header in ["cxxabi.h", "__cxxabi_config.h"] {
         copy(
            src.join("libcxxabi/include").join(header),
            format!("{inc}/{header}"),
         )?;
      }
      copy_tree(&src.join("libunwind/include"), Path::new(inc), "**/*.h")
   }

   fn items(lib: &Lib, obj: &str, posix: bool) -> io::Result<Vec<Item>> {
      let mut found = files(&format!("{}/{}", lib.dir, lib.glob))?;
      if !posix && let Some(glob) = lib.glob_windows {
         found.extend(files(&format!("{}/{glob}", lib.dir))?);
      }
      let sources = found
         .iter()
         .map(|path| path.to_string_lossy().into_owned())
         .filter(|path| {
            path
               .strip_prefix(&format!("{}/", lib.dir))
               .is_none_or(|rel| {
                  !lib.skip.contains(&rel) && (posix || !lib.skip_windows.contains(&rel))
               })
         })
         .chain(lib.extra.iter().map(|extra| (*extra).to_owned()));
      Ok(sources
         .map(|source| {
            let extension = Path::new(&source)
               .extension()
               .map(OsStr::to_ascii_lowercase);
            let std = match extension.as_deref().and_then(|ext| ext.to_str()) {
               Some("cpp") => vec![format!("-std={}", lib.std)],
               Some("c") => vec!["-std=c11".to_owned()],
               _ => Vec::new(),
            };
            Item {
               obj:   PathBuf::from(format!("{obj}/{}/{source}.o", lib.name)),
               src:   PathBuf::from(source),
               flags: std,
            }
         })
         .collect())
   }

   fn flags(stage: &Stage, out: &str) -> io::Result<(Vec<String>, Vec<String>)> {
      let profile = stage.profile()?;
      let mut common = stage.ccflags()?;
      common.extend(profile.pic().iter().map(|flag| (*flag).to_owned()));
      common.extend(
         [
            "-O2",
            "-nostdinc++",
            "-fvisibility-inlines-hidden",
            "-ffunction-sections",
            "-fdata-sections",
            "-funwind-tables",
            "-DNDEBUG",
            "-D_LIBCPP_HAS_NO_PRAGMA_SYSTEM_HEADER",
            "-D__STDC_CONSTANT_MACROS",
            "-D__STDC_FORMAT_MACROS",
            "-D__STDC_LIMIT_MACROS",
         ]
         .into_iter()
         .map(str::to_owned),
      );
      common.push(format!("-I{out}/include/c++/v1"));
      common.extend(
         ["-Ilibcxxabi/include", "-Ilibunwind/include", "-w"]
            .into_iter()
            .map(str::to_owned),
      );
      let mut link = stage.ccflags()?;
      link.extend(["-nostdlib++", "-shared"].into_iter().map(str::to_owned));
      link.push(format!("-L{out}/lib"));
      if profile.posix() {
         link.push("-Wl,-z,defs".to_owned());
      }
      Ok((common, link))
   }

   fn link_names(out: &str) -> io::Result<()> {
      for lib in ["unwind", "c++abi", "c++"] {
         symlink(format!("lib{lib}.so.1"), format!("{out}/lib/lib{lib}.so"))?;
      }
      write_file(
         Path::new(&format!("{out}/lib/libgcc_s.so")),
         b"INPUT(-lunwind)\n",
         None,
      )?;
      symlink("libunwind.a", format!("{out}/lib/libgcc_s.a"))?;
      write_file(
         Path::new(&format!("{out}/lib/libatomic.so")),
         b"/* compiler-rt builtins */\n",
         None,
      )
   }

   fn build_lib(
      lib: &Lib,
      stage: &Stage,
      common: &[String],
      link: &[String],
      built: &BTreeMap<&str, Built>,
   ) -> io::Result<Built> {
      let out = stage.out();
      let profile = stage.profile()?;
      let coff = stage.binfmt == "coff";
      let mut items = Self::items(lib, &format!("{}/obj", stage.build_top), profile.posix())?;
      say(&format!("lib{}: {} files", lib.name, items.len()));
      let mut flags = common.to_vec();
      flags.extend(lib.flags.iter().map(|flag| (*flag).to_owned()));
      if profile.posix() {
         flags.extend(lib.posix_flags.iter().map(|flag| (*flag).to_owned()));
      }
      let mut static_flags = flags.clone();
      if coff {
         static_flags.extend([
            "-D_LIBCPP_DISABLE_VISIBILITY_ANNOTATIONS".to_owned(),
            "-D_LIBCXXABI_DISABLE_VISIBILITY_ANNOTATIONS".to_owned(),
            "-D_LIBUNWIND_HIDE_SYMBOLS".to_owned(),
         ]);
      }
      let objects = compile(&static_flags, &items)?;
      let dll = if coff {
         for item in &mut items {
            item.obj = PathBuf::from(format!("{}.dll.o", item.obj.display()));
         }
         compile(&flags, &items)?
      } else {
         Vec::new()
      };
      let below: &[&str] = match lib.name {
         "c++abi" => &["unwind"],
         "c++" => &["unwind", "c++abi"],
         _ => &[],
      };
      let members: Vec<PathBuf> = objects
         .iter()
         .chain(below.iter().flat_map(|layer| &built[layer].objects))
         .cloned()
         .collect();
      archive(Path::new(&format!("{out}/lib/lib{}.a", lib.name)), &members)?;
      if let Some(so_libs) = lib.so
         && profile.posix()
      {
         let so = format!("lib{}.so.1", lib.name);
         let mut args = link.to_vec();
         args.extend([
            format!("-Wl,-soname,{so}"),
            "-o".to_owned(),
            format!("{out}/lib/{so}.0"),
         ]);
         args.extend(
            members
               .iter()
               .map(|path| path.to_string_lossy().into_owned()),
         );
         args.extend(so_libs.iter().map(|flag| (*flag).to_owned()));
         run("clang", &strs(&args))?;
         symlink(format!("{so}.0"), format!("{out}/lib/{so}"))?;
      } else if coff && lib.dll {
         create_dir_all(format!("{out}/bin"))?;
         let mut args = link.to_vec();
         args.extend([
            "-o".to_owned(),
            format!("{out}/bin/lib{}.dll", lib.name),
            format!("-Wl,--out-implib,{out}/lib/lib{}.dll.a", lib.name),
         ]);
         args.extend(dll.iter().map(|path| path.to_string_lossy().into_owned()));
         if lib.name == "c++" {
            args.extend(
               below
                  .iter()
                  .flat_map(|layer| &built[layer].dll)
                  .map(|path| path.to_string_lossy().into_owned()),
            );
            args.push("-lunwind".to_owned());
         }
         run("clang", &strs(&args))?;
      }
      Ok(Built { objects, dll })
   }
}

impl Recipe for Runtimes {
   fn name(&self) -> &'static str {
      "runtimes"
   }

   /// libunwind, libc++abi, libc++ (static and shared) and libc++experimental
   /// (static) without cmake, the flags and `__config_site` are what its
   /// configure produces. libc++ 21 includes llvm-libc's internal headers
   /// (libc/shared, `from_chars`), the rest of the monorepo stays packed. Each
   /// static archive carries the layers below it (upstream's
   /// `LIBCXX{,ABI}_STATICALLY_LINK_UNWINDER_IN_STATIC_LIBRARY`), so
   /// `-static -lc++` needs no `-lc++abi -lunwind`. libc++.so.1 carries c++abi
   /// and unwind, so no `INPUT()` script as upstream installs, zig's linker
   /// resolves script members in its own -L list only. rustc's and go's
   /// prebuilt std hard-code `-lgcc_s` for the unwinder and libunwind has the
   /// same `_Unwind_*` ABI. GCC-world build files add -latomic for `__atomic_*`
   /// libcalls, which compiler-rt's always linked builtins provide, so the
   /// name only has to resolve.
   fn run(&self, stage: &Stage) -> io::Result<()> {
      let out = stage.out();
      let src = stage.unpack("llvm", &[
         "libcxx",
         "libcxxabi",
         "libunwind",
         "compiler-rt/lib/builtins",
         "libc/shared",
         "libc/src/__support",
         "libc/include",
         "libc/hdr",
         "runtimes",
         "cmake",
      ])?;
      Self::install_headers(stage, &src, &format!("{out}/include"))?;
      create_dir_all(format!("{out}/lib"))?;
      set_current_dir(&src)?;
      let (common, link) = Self::flags(stage, out)?;

      let mut built = BTreeMap::new();
      for lib in &LIBS {
         let compiled = Self::build_lib(lib, stage, &common, &link, &built)?;
         built.insert(lib.name, compiled);
      }
      if stage.profile()?.posix() {
         Self::link_names(out)?;
      }
      Ok(())
   }
}
