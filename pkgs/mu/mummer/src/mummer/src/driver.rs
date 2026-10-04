use std::{
   collections::HashMap,
   env::{
      current_exe,
      var,
   },
   fs::{
      File,
      canonicalize,
      metadata,
      read,
      read_to_string,
   },
   io::{
      self,
      Read as _,
   },
   os::unix::fs::PermissionsExt as _,
   path::Path,
};

use mummer::{
   elf::{
      RUNPATH_SLACK,
      is_elf,
      needed,
      runpath_text,
      segments,
   },
   files::write_file,
   image::Image,
   store::is_store_path,
};
use serde::Deserialize;

use crate::manifest::parse_depfile;

#[derive(Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum BinaryFormat {
   #[default]
   Elf,
   MachO,
   Coff,
}

impl BinaryFormat {
   fn keeps(self, arg: &str) -> Option<String> {
      match self {
         Self::Elf => {
            (!arg.starts_with("-Wl,--dynamic-linker") && !arg.starts_with("-Wl,-dynamic-linker"))
               .then(|| arg.to_owned())
         },
         Self::MachO => Some(arg.to_owned()),
         Self::Coff => {
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
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Language {
   Cc,
   Cxx,
   Fortran,
}

#[derive(Default, Deserialize)]
pub struct PackageFlags {
   #[serde(default)]
   cflags:   Vec<String>,
   #[serde(default)]
   cxxflags: Vec<String>,
   #[serde(default)]
   ldflags:  Vec<String>,
}

#[derive(Deserialize)]
#[serde(default)]
pub struct Driver {
   #[serde(rename = "cc")]
   compiler:    String,
   fc:          String,
   #[serde(rename = "binfmt")]
   format:      BinaryFormat,
   #[serde(skip)]
   configured:  bool,
   libc:        String,
   #[serde(rename = "interp")]
   interpreter: String,
   crt:         String,
   flags:       Vec<String>,
   cxxflags:    Vec<String>,
   fflags:      Vec<String>,
   #[serde(rename = "prefix-map")]
   prefix_map:  Vec<String>,
   #[serde(skip)]
   package:     PackageFlags,
}

impl Default for Driver {
   fn default() -> Self {
      Self {
         compiler:    String::new(),
         fc:          String::new(),
         format:      BinaryFormat::Elf,
         configured:  false,
         libc:        String::new(),
         interpreter: "ld-linux-x86-64.so.2".to_owned(),
         crt:         String::new(),
         flags:       Vec::new(),
         cxxflags:    Vec::new(),
         fflags:      Vec::new(),
         prefix_map:  Vec::new(),
         package:     PackageFlags::default(),
      }
   }
}

impl Driver {
   pub fn compiler(&self, language: Language) -> &str {
      match language {
         Language::Cc | Language::Cxx => &self.compiler,
         Language::Fortran => &self.fc,
      }
   }

   pub const fn format(&self) -> BinaryFormat {
      self.format
   }
   pub fn load() -> io::Result<Self> {
      let executable = current_exe()?;
      let root = executable
         .parent()
         .and_then(Path::parent)
         .ok_or_else(|| io::Error::other("mummer has no installation root"))?;
      let mut driver = Self::default();

      match read_to_string(root.join("etc/mummer.json")) {
         Ok(config_text) => {
            driver = serde_json::from_str(&config_text)
               .map_err(|error| io::Error::other(format!("invalid mummer.json, {error}")))?;
            driver.configured = true;

            for flag in &mut driver.fflags {
               if let Some(position) = flag.find("@/")
                  && (position == 0
                     || (position == 2 && flag.starts_with('-'))
                     || flag.as_bytes().get(position - 1) == Some(&b'='))
               {
                  flag.replace_range(position..=position, &root.to_string_lossy());
               }
            }

            if let Ok(packages_text) = var("PKGS_CC") {
               let mut packages =
                  serde_json::from_str::<HashMap<String, PackageFlags>>(&packages_text)?;
               let name = root
                  .to_str()
                  .ok_or_else(|| io::Error::other("mummer installation path is not UTF-8"))?;
               driver.package = packages.remove(name).unwrap_or_default();
            }
         },
         Err(error) if error.kind() == io::ErrorKind::NotFound => {
            driver.compiler = var("MUMMER_CC").unwrap_or_default();
         },
         Err(error) => return Err(error),
      }
      if driver.compiler.is_empty() {
         return Err(io::Error::other(
            "mummer needs etc/mummer.json or MUMMER_CC",
         ));
      }
      Ok(driver)
   }

   #[expect(
      clippy::too_many_lines,
      reason = "compiler mode rewrite is linear, each case is in one place"
   )]
   pub fn arguments(&self, language: Language, raw: &[String]) -> Vec<String> {
      if !self.configured {
         return raw.to_vec();
      }
      let cxx = language == Language::Cxx;
      let fortran = language == Language::Fortran;
      let mut user = Vec::new();
      let mut linking = true;
      let mut have_input = false;
      let mut shared = false;
      let mut no_policy = false;
      let mut optimizes = true;
      let mut sets_fortify = false;
      let mut index = 0;
      while index < raw.len() {
         let arg = &raw[index];
         index += 1;
         let Some(kept) = self.format.keeps(arg) else {
            continue;
         };
         linking &= !matches!(
            arg.as_str(),
            "-c" | "-E" | "-S" | "-M" | "-MM" | "-fsyntax-only" | "-###"
         );
         no_policy |= matches!(
            arg.as_str(),
            "-static" | "-static-pie" | "-r" | "-Wl,-r" | "-nostartfiles"
         );
         shared |= arg == "-shared" || arg == "-dynamiclib";
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
      output.extend_from_slice(if fortran { &self.fflags } else { &self.flags });

      if !fortran {
         output.extend(
            self
               .package
               .cflags
               .iter()
               .filter(|flag| !fortify(flag) || (!sets_fortify && optimizes))
               .cloned(),
         );
      }
      if cxx {
         output.push("--driver-mode=g++".to_owned());
         output.extend_from_slice(&self.cxxflags);
         output.extend_from_slice(&self.package.cxxflags);
      }
      if !fortran {
         for mapping in self.prefix_map.iter().map(String::as_str).chain(
            var("PKGS_PREFIX_MAP")
               .unwrap_or_default()
               .split(':')
               .filter(|part| !part.is_empty()),
         ) {
            output.push(format!("-ffile-prefix-map={mapping}"));
         }
      }
      if self.format == BinaryFormat::Elf && linking && have_input {
         output.extend([
            "-Wl,--build-id=sha1".to_owned(),
            r#"-Wl,--package-metadata={"type":"motley"}"#.to_owned(),
         ]);
      }
      if !fortran {
         output.push("--end-no-unused-arguments".to_owned());
      }

      if linking && have_input && self.format == BinaryFormat::Coff {
         self.lower_libraries(&mut user);
      }

      output.extend_from_slice(&user);
      if linking && have_input {
         output.extend_from_slice(&self.package.ldflags);
      }
      if !linking || !have_input || no_policy {
         return output;
      }
      match self.format {
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
         "-Wl,--enable-new-dtags".to_owned(),
      ]);
      if shared {
         return output;
      }
      if self.crt.is_empty() {
         output.push(format!(
            "-Wl,--dynamic-linker={}/lib/{}",
            self.libc, self.interpreter
         ));
      } else {
         output.extend([
            "-x".to_owned(),
            "none".to_owned(),
            self.crt.clone(),
            format!(
               "-Wl,--dynamic-linker={}/lib/{}{}",
               self.libc,
               "./".repeat(12),
               self.interpreter
            ),
            "-Wl,--export-dynamic-symbol=__reloc_start".to_owned(),
         ]);
      }
      output
   }

   fn lower_libraries(&self, user: &mut [String]) {
      let mut directories = Vec::new();

      for args in [&*user, self.package.ldflags.as_slice()] {
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
/// rewrites into. Some(room) when the placeholder is too small for the output's
/// `DT_NEEDED` count, the link to redo with `reserve`.
pub fn settle_runpath(output: &Path, dependencies: &str) -> io::Result<Option<usize>> {
   let mut image = Image {
      bytes: read(output)?,
   };
   if !is_elf(&image) {
      return Ok(None);
   }
   let segments = segments(&image)?;
   let Some(text) = runpath_text(&image, &segments)? else {
      return Ok(None);
   };
   if !text.value.split(':').any(is_placeholder) {
      return Ok(None);
   }
   let given = text
      .value
      .split(':')
      .filter(|entry| !entry.is_empty() && !is_placeholder(entry) && !host_directory(entry))
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
      let Some(parent) = canonicalize(&input)?.parent().map(Path::to_path_buf) else {
         continue;
      };
      let directory = parent.to_string_lossy().into_owned();
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
      return Ok(Some((wanted + 1).next_power_of_two()));
   }
   path.push('/');
   path.push_str(&"_".repeat(text.capacity - 1 - path.len()));
   image.write(text.offset, path.as_bytes())?;
   let mode = metadata(output)?.permissions().mode();
   write_file(output, &image.bytes, Some(mode & 0o7777))?;
   Ok(None)
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
