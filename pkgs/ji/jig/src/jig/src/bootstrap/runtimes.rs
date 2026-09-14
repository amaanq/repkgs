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

use jig::files::write_file;

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
   name:  &'static str,
   dir:   &'static str,
   glob:  &'static str,
   skip:  &'static [&'static str],
   extra: &'static [&'static str],
   std:   &'static str,
   so:    Option<&'static [&'static str]>,
   flags: &'static [&'static str],
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
      name:  "unwind",
      dir:   "libunwind/src",
      glob:  "*.{cpp,c,S}",
      skip:  &["Unwind_AIXExtras.cpp"],
      extra: &["compiler-rt/lib/builtins/gcc_personality_v0.c"],
      std:   "c++17",
      so:    Some(&["-lc"]),
      flags: &[
         "-D_LIBUNWIND_IS_NATIVE_ONLY",
         "-D_LIBUNWIND_LINK_DL_LIB",
         "-D_LIBUNWIND_LINK_PTHREAD_LIB",
         "-fno-exceptions",
         "-fno-rtti",
         "-fstrict-aliasing",
      ],
   },
   Lib {
      name:  "c++abi",
      dir:   "libcxxabi/src",
      glob:  "*.cpp",
      skip:  &["cxa_noexception.cpp"],
      extra: &[],
      std:   "c++26",
      so:    Some(&["-lunwind", "-lc"]),
      flags: &[
         "-D_LIBCPP_BUILDING_LIBRARY",
         "-D_LIBCXXABI_BUILDING_LIBRARY",
         "-D_LIBCXXABI_LINK_PTHREAD_LIB",
         "-DLIBCXX_BUILDING_LIBCXXABI",
         "-fstrict-aliasing",
         "-fsized-deallocation",
         "-Ilibcxx/src",
      ],
   },
   Lib {
      name:  "c++",
      dir:   "libcxx/src",
      glob:  "{*,filesystem/*,ryu/*,pstl/*}.cpp",
      skip:  &[
         "new.cpp",
         "filesystem/int128_builtins.cpp",
         "pstl/libdispatch.cpp",
      ],
      extra: &[],
      std:   "c++26",
      so:    Some(&["-lc++abi", "-lunwind", "-lc"]),
      flags: &[
         "-D_LIBCPP_BUILDING_LIBRARY",
         "-D_LIBCPP_LINK_PTHREAD_LIB",
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
   },
   Lib {
      name:  "c++experimental",
      dir:   "libcxx/src/experimental",
      glob:  "*.cpp",
      skip:  &[],
      extra: &[],
      std:   "c++26",
      so:    None,
      flags: &[
         "-D_LIBCPP_BUILDING_LIBRARY",
         "-D_LIBCPP_LINK_RT_LIB",
         "-D_LIBCPP_REMOVE_TRANSITIVE_INCLUDES",
         "-DLIBCXX_BUILDING_LIBCXXABI",
         "-D_LIBCPP_LINK_PTHREAD_LIB",
         "-D_LIBCPP_ENABLE_EXPERIMENTAL",
         "-fvisibility=hidden",
         "-faligned-allocation",
         "-fsized-deallocation",
      ],
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
   fn config_site(libc: &str) -> String {
      let musl = u8::from(libc == "musl");
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
#define _LIBCPP_HAS_TIME_ZONE_DATABASE 1
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
         Self::config_site(&stage.libc).as_bytes(),
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

   fn items(lib: &Lib, obj: &str) -> io::Result<Vec<Item>> {
      let found = files(&format!("{}/{}", lib.dir, lib.glob))?;
      let sources = found
         .iter()
         .map(|path| path.to_string_lossy().into_owned())
         .filter(|path| {
            path
               .strip_prefix(&format!("{}/", lib.dir))
               .is_none_or(|rel| !lib.skip.contains(&rel))
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
      let mut common = stage.ccflags()?;
      common.extend(
         [
            "-O2",
            "-fPIC",
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
      link.extend(
         ["-nostdlib++", "-shared", "-Wl,-z,defs"]
            .into_iter()
            .map(str::to_owned),
      );
      link.push(format!("-L{out}/lib"));
      Ok((common, link))
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
      let obj = format!("{}/obj", stage.build_top);
      Self::install_headers(stage, &src, &format!("{out}/include"))?;
      create_dir_all(format!("{out}/lib"))?;
      set_current_dir(&src)?;
      let (common, link) = Self::flags(stage, out)?;

      let mut built: BTreeMap<&str, Vec<PathBuf>> = BTreeMap::new();
      for lib in &LIBS {
         let items = Self::items(lib, &obj)?;
         say(&format!("lib{}: {} files", lib.name, items.len()));
         let mut flags = common.clone();
         flags.extend(lib.flags.iter().map(|flag| (*flag).to_owned()));
         let objs = compile(&flags, &items)?;
         let below: &[&str] = match lib.name {
            "c++abi" => &["unwind"],
            "c++" => &["unwind", "c++abi"],
            _ => &[],
         };
         let carried: Vec<PathBuf> = below
            .iter()
            .flat_map(|layer| built.get(layer).cloned().unwrap_or_default())
            .collect();
         let mut members = objs.clone();
         members.extend(carried.iter().cloned());
         archive(Path::new(&format!("{out}/lib/lib{}.a", lib.name)), &members)?;
         if let Some(so_libs) = lib.so {
            let so = format!("lib{}.so.1", lib.name);
            let soname = format!("-Wl,-soname,{so}");
            let output = format!("{out}/lib/{so}.0");
            let mut args: Vec<&str> = strs(&link);
            args.push(&soname);
            args.push("-o");
            args.push(&output);
            let object_texts: Vec<String> = members
               .iter()
               .map(|path| path.to_string_lossy().into_owned())
               .collect();
            args.extend(strs(&object_texts));
            args.extend(so_libs.iter().copied());
            run("clang", &args)?;
            symlink(format!("{so}.0"), format!("{out}/lib/{so}"))?;
         }
         built.insert(lib.name, objs);
      }
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
}
