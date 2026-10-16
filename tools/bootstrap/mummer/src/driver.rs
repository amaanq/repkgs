use std::{
   convert::Infallible,
   env::{
      current_exe,
      var,
   },
   ffi::OsString,
   fs::{
      File,
      canonicalize,
      read_to_string,
   },
   io::{
      self,
      Read as _,
   },
   os::unix::process::CommandExt as _,
   path::Path,
   process::Command,
};

use mummer::{
   config::{
      BinaryFormat,
      Config,
      PackageFlags,
      package_flags,
   },
   elf::{
      RUNPATH_SLACK,
      is_elf,
      needed,
      runpath_text,
      segments,
   },
   files::utf8,
   image::Image,
   store::{
      Store,
      is_store_path,
   },
};

use crate::manifest::{
   Miss,
   parse_depfile,
};

fn kept(format: BinaryFormat, arg: &str) -> Option<String> {
   match format {
      BinaryFormat::Elf => {
         (!arg.starts_with("-Wl,--dynamic-linker") && !arg.starts_with("-Wl,-dynamic-linker"))
            .then(|| arg.to_owned())
      },
      BinaryFormat::MachO => Some(arg.to_owned()),
      BinaryFormat::Coff => {
         if matches!(
            arg,
            "-fPIC" | "-fpic" | "-fPIE" | "-fpie" | "-fno-PIC" | "-fno-pic" | "-pie"
         ) {
            return None;
         }
         if (arg.starts_with("-std=c") || arg.starts_with("-std=gnu"))
            && ["++98", "++03", "++0x", "++11"]
               .iter()
               .any(|suffix| arg.ends_with(suffix))
         {
            return Some(format!("{}14", &arg[..arg.len() - 2]));
         }
         Some(arg.to_owned())
      },
   }
}

/// Clang's own spelling of a driver flag its Options.td also takes with `--`.
pub fn canonical(arg: &str) -> &str {
   match arg {
      "--shared" => "-shared",
      "--static" => "-static",
      "--compile" => "-c",
      "--assemble" => "-S",
      "--preprocess" => "-E",
      "--dependencies" => "-M",
      "--user-dependencies" => "-MM",
      other => other,
   }
}

pub fn link_tool(arg: &str) -> Option<&str> {
   arg.strip_prefix("--ld-path=")
      .or_else(|| arg.strip_prefix("-Wl,-plugin,"))
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Language {
   Cc,
   Cxx,
   Fortran,
}

pub struct Toolchain {
   config:  Config,
   package: PackageFlags,
}

pub enum Driver {
   Bare(String),
   Configured(Box<Toolchain>),
}

impl Driver {
   /// The C compiler, which also compiles C++ under its g++ driver mode.
   pub fn cc(&self) -> &str {
      match *self {
         Self::Bare(ref compiler) => compiler,
         Self::Configured(ref toolchain) => &toolchain.config.cc,
      }
   }

   pub fn compiler(&self, language: Language) -> Option<&str> {
      match language {
         Language::Cc | Language::Cxx => Some(self.cc()),
         Language::Fortran => self.toolchain()?.config.fc.as_deref(),
      }
   }

   fn toolchain(&self) -> Option<&Toolchain> {
      match *self {
         Self::Bare(_) => None,
         Self::Configured(ref toolchain) => Some(toolchain),
      }
   }

   /// Replaces this process with the program the toolchain runs as `name`.
   pub fn exec_tool(&self, name: &str, args: &[OsString]) -> io::Result<Infallible> {
      let tool = self
         .toolchain()
         .and_then(|toolchain| toolchain.config.tools.get(name))
         .ok_or_else(|| io::Error::other(format!("mummer.json names no {name}")))?;
      Err(
         Command::new(&tool.program)
            .args(&tool.before)
            .args(args)
            .args(&tool.after)
            .exec(),
      )
   }

   /// The libc a configured ELF toolchain builds against, its sysroot.
   pub fn libc(&self) -> Option<&str> {
      Some(self.toolchain()?.config.policy.as_ref()?.libc.as_str())
   }

   pub fn ldflags(&self) -> &[String] {
      self
         .toolchain()
         .map_or(&[], |toolchain| toolchain.package.ldflags.as_slice())
   }

   pub fn format(&self) -> BinaryFormat {
      self
         .toolchain()
         .map_or(BinaryFormat::Elf, |toolchain| toolchain.config.binfmt)
   }

   pub fn load() -> io::Result<Self> {
      let executable = current_exe()?;
      Self::load_at(
         executable
            .parent()
            .and_then(Path::parent)
            .ok_or_else(|| io::Error::other("mummer has no installation root"))?,
      )
   }

   /// The driver of the toolchain installed at `root`.
   pub fn load_at(root: &Path) -> io::Result<Self> {
      let text = match read_to_string(root.join("etc/mummer.json")) {
         Ok(text) => text,
         Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return var("MUMMER_CC")
               .ok()
               .filter(|compiler| !compiler.is_empty())
               .map(Self::Bare)
               .ok_or_else(|| io::Error::other("mummer needs etc/mummer.json or MUMMER_CC"));
         },
         Err(error) => return Err(error),
      };
      let mut config = serde_json::from_str::<Config>(&text)
         .map_err(|error| io::Error::other(format!("invalid mummer.json, {error}")))?;
      if config.cc.is_empty() {
         return Err(io::Error::other("mummer.json names no cc"));
      }
      for flag in &mut config.fflags {
         if let Some(position) = flag.find("@/")
            && (position == 0
               || (position == 2 && flag.starts_with('-'))
               || flag.as_bytes().get(position - 1) == Some(&b'='))
         {
            flag.replace_range(position..=position, utf8(root)?);
         }
      }
      let name = root
         .to_str()
         .ok_or_else(|| io::Error::other("mummer installation path is not UTF-8"))?;
      let package = package_flags()?.remove(name).unwrap_or_default();
      Ok(Self::Configured(Box::new(Toolchain { config, package })))
   }

   #[expect(
      clippy::too_many_lines,
      reason = "compiler mode rewrite is linear, each case is in one place"
   )]
   pub fn arguments(&self, language: Language, raw: &[String]) -> Vec<String> {
      let Some(toolchain) = self.toolchain() else {
         return raw.to_vec();
      };
      let config = &toolchain.config;
      let package = &toolchain.package;
      let format = config.binfmt;
      let cxx = language == Language::Cxx;
      let fortran = language == Language::Fortran;
      let mut user = Vec::new();
      let mut linking = true;
      let mut have_input = false;
      let mut shared = false;
      let mut no_policy = false;
      let mut no_startup = false;
      let mut optimizes = true;
      let mut sets_fortify = false;
      let mut index = 0;
      while index < raw.len() {
         let arg = &raw[index];
         index += 1;
         let Some(kept) = kept(format, arg) else {
            continue;
         };
         let mode = canonical(arg);
         linking &= !matches!(
            mode,
            "-c" | "-E" | "-S" | "-M" | "-MM" | "-fsyntax-only" | "-###" | "--driver-mode=cpp"
         );
         no_policy |= matches!(mode, "-static" | "-static-pie" | "-r" | "-Wl,-r");
         no_startup |= mode == "-nostartfiles";
         shared |= mode == "-shared" || mode == "-dynamiclib";
         have_input |= !arg.starts_with('-');
         sets_fortify |= fortify(arg);
         if arg.starts_with("-O") {
            optimizes = arg != "-O0";
         }
         user.push(kept);
      }
      let mut output = vec![if fortran {
         "-Qunused-arguments".to_owned()
      } else {
         "--start-no-unused-arguments".to_owned()
      }];
      output.extend_from_slice(if fortran {
         &config.fflags
      } else {
         &config.flags
      });

      if !fortran {
         output.extend(
            package
               .cflags
               .iter()
               .filter(|flag| !fortify(flag) || (!sets_fortify && optimizes))
               .cloned(),
         );
      }
      if cxx {
         output.push("--driver-mode=g++".to_owned());
         output.extend_from_slice(&config.cxxflags);
         output.extend_from_slice(&package.cxxflags);
      }
      if !fortran {
         for mapping in config.prefix_map.iter().map(String::as_str).chain(
            var("PKGS_PREFIX_MAP")
               .unwrap_or_default()
               .split(':')
               .filter(|part| !part.is_empty()),
         ) {
            output.push(format!("-ffile-prefix-map={mapping}"));
         }
      }
      if format == BinaryFormat::Elf && linking && have_input {
         output.extend([
            "-Wl,--build-id=sha1".to_owned(),
            r#"-Wl,--package-metadata={"type":"motley"}"#.to_owned(),
         ]);
      }
      if !fortran {
         output.push("--end-no-unused-arguments".to_owned());
      }

      if linking && have_input && format == BinaryFormat::Coff {
         lower_libraries(&mut user, &package.ldflags);
      }

      output.extend_from_slice(&user);
      if linking && have_input {
         output.extend_from_slice(&package.ldflags);
      }
      if !linking || !have_input || no_policy {
         return output;
      }
      match format {
         BinaryFormat::Elf => {},
         BinaryFormat::MachO => {
            output.push("-Wl,-headerpad_max_install_names".to_owned());
            return output;
         },
         BinaryFormat::Coff => return output,
      }

      let path = placeholder(RUNPATH_RESERVE);
      output.extend([
         "-Wl,--undefined-version".to_owned(),
         format!("-Wl,-rpath,{path}"),
      ]);
      if !disables_new_dtags(&user) {
         output.push("-Wl,--enable-new-dtags".to_owned());
      }

      let Some(ref policy) = config.policy else {
         return output;
      };
      if shared || no_startup {
         return output;
      }
      if let Some(ref crt) = policy.crt {
         output.extend([
            "-x".to_owned(),
            "none".to_owned(),
            crt.clone(),
            format!(
               "-Wl,--dynamic-linker={}/lib/{}{}",
               policy.libc,
               "./".repeat(12),
               policy.interp
            ),
            "-Wl,--export-dynamic-symbol=__reloc_start".to_owned(),
         ]);
      } else {
         output.push(format!(
            "-Wl,--dynamic-linker={}/lib/{}",
            policy.libc, policy.interp
         ));
      }
      output
   }
}

fn lower_libraries(user: &mut [String], ldflags: &[String]) {
   let mut directories = Vec::new();

   for args in [&*user, ldflags] {
      for (position, arg) in args.iter().enumerate() {
         if let Some(tail) = arg.strip_prefix("-L") {
            let directory = if tail.is_empty() {
               args.get(position + 1).map(String::as_str)
            } else {
               Some(tail)
            };

            if let Some(path) = directory {
               directories.push(Path::new(path).to_path_buf());
            }
         }
      }
   }

   for arg in user {
      let Some(name) = arg.strip_prefix("-l") else {
         continue;
      };

      if arg.contains('/')
         || directories.iter().any(|directory| {
            directory.join(format!("lib{name}.dll.a")).exists()
               || directory.join(format!("lib{name}.a")).exists()
               || directory.join(name).exists()
         })
      {
         continue;
      }

      arg.make_ascii_lowercase();
   }
}

fn host_directory(path: &str) -> bool {
   ["/usr", "/lib", "/opt"]
      .iter()
      .any(|prefix| path.starts_with(prefix))
}

fn fortify(arg: &str) -> bool {
   arg.starts_with("-D_FORTIFY_SOURCE")
      || arg == "-U_FORTIFY_SOURCE"
      || arg.starts_with("-Wp,-D_FORTIFY_SOURCE")
      || arg.starts_with("-Wp,-U_FORTIFY_SOURCE")
}

/// The space a link reserves for its RUNPATH, enough for most outputs. One
/// that needs more is linked again with room for exactly what it needs.
const RUNPATH_RESERVE: usize = 4096;

/// Whether `args` ask the linker for `DT_RPATH` with `--disable-new-dtags`.
fn disables_new_dtags(args: &[String]) -> bool {
   args.iter().enumerate().any(|(index, arg)| {
      arg.strip_prefix("-Wl,")
         .is_some_and(|list| list.split(',').any(|flag| flag == "--disable-new-dtags"))
         || (arg == "--disable-new-dtags" && index > 0 && args[index - 1] == "-Xlinker")
   })
}

/// The RUNPATH entry a link reserves, padding `settle_runpath` fills in.
fn placeholder(room: usize) -> String {
   format!("/{}", "_".repeat(room - 1))
}

fn is_placeholder(entry: &str) -> bool {
   entry.len() > 1 && entry.starts_with('/') && entry[1..].bytes().all(|byte| byte == b'_')
}

/// Gives the placeholder in `args` `room` bytes, for a second link.
pub fn reserve(args: &mut [String], room: usize) {
   for arg in args {
      if arg.strip_prefix("-Wl,-rpath,").is_some_and(is_placeholder) {
         *arg = format!("-Wl,-rpath,{}", placeholder(room));
      }
   }
}

/// Rewrites the output's RUNPATH as the rpaths the link was given, less host
/// directories, then the directories of the shared libraries the linker
/// opened, as its dependency file names them, so a library the link named but
/// `--as-needed` dropped still loads by name, keeping the padding reloc-fixup
/// rewrites into. Room when the placeholder is too small for the output's
/// `DT_NEEDED` count, the link to redo with `reserve`.
pub fn settle_runpath(bytes: &[u8], dependencies: &str) -> io::Result<Settled> {
   let mut image = Image {
      bytes: bytes.to_vec(),
   };
   if !is_elf(&image) {
      return Ok(Settled::Image(image.bytes));
   }
   let segments = segments(&image)?;
   let Some(text) = runpath_text(&image, &segments)? else {
      return Ok(Settled::Image(image.bytes));
   };
   if !text.value.split(':').any(is_placeholder) {
      return Ok(Settled::Image(image.bytes));
   }
   let given = text
      .value
      .split(':')
      .filter(|entry| !is_placeholder(entry) && !host_directory(entry))
      .collect::<Vec<_>>();
   let mut path = String::new();
   for entry in &given {
      path.push_str(entry);
      path.push(':');
   }
   let needed = needed(&image, &segments)?;
   let mut directories = Vec::new();
   for input in parse_depfile(dependencies) {
      if !is_store_path(&input) || !shared_object(Path::new(&input))? {
         continue;
      }
      // the directory the linker found it in, not where its symlink leads, so
      // libc keeps the sysroot's libunwind beside it for $ORIGIN
      let Some(parent) = Path::new(&input).parent().map(canonicalize).transpose()? else {
         continue;
      };
      let directory = utf8(&parent)?.to_owned();
      if !host_directory(&directory) && !directories.contains(&directory) {
         directories.push(directory);
      }
   }
   for directory in &directories {
      path.push_str(directory);
      path.push_str("/.:");
   }
   let entries = directories.len() + given.len();
   let wanted = path.len() + 1 + entries.max(1) * RUNPATH_SLACK + needed.len() * 80;
   if wanted >= text.capacity {
      return Ok(Settled::Room((wanted + 1).next_power_of_two()));
   }
   path.push('/');
   path.push_str(&"_".repeat(text.capacity - 1 - path.len()));
   image.write(text.offset, path.as_bytes())?;
   Ok(Settled::Image(image.bytes))
}

/// A stored link output moved into this build's store paths, its RUNPATH
/// settled from the link's dependency file when it was stored unsettled.
pub fn replayed_link(
   store: &Store,
   mut bytes: Vec<u8>,
   dependencies: Option<&str>,
) -> Result<Vec<u8>, Miss> {
   store.mask_output(&mut bytes, true);
   store.resolve_bytes(&mut bytes);
   let Some(text) = dependencies else {
      return Ok(bytes);
   };
   match settle_runpath(&bytes, text)? {
      Settled::Image(settled) => Ok(settled),
      Settled::Room(_) => Err(Miss::RunpathRoom),
   }
}

/// What settling an image came to.
pub enum Settled {
   /// The image, its RUNPATH settled if it had a placeholder.
   Image(Vec<u8>),
   /// The room the placeholder needs, for a link redone with `reserve`.
   Room(usize),
}

/// An ELF shared object, what the dynamic loader finds by RUNPATH.
fn shared_object(path: &Path) -> io::Result<bool> {
   let mut header = [0_u8; 18];
   match File::open(path).and_then(|mut file| file.read_exact(&mut header)) {
      Ok(()) => {
         Ok(header.starts_with(b"\x7fELF") && u16::from_le_bytes([header[16], header[17]]) == 3)
      },
      Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => Ok(false),
      Err(error) => Err(error),
   }
}
