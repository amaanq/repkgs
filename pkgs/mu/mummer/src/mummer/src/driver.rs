use std::{
   collections::HashMap,
   env::{
      current_exe,
      var,
   },
   fs::{
      canonicalize,
      read_to_string,
   },
   io,
   mem,
   path::Path,
   process::Command,
};

use mummer::{
   elf::{
      RUNPATH_SLACK,
      shared_library,
   },
   store::is_store_path,
};
use serde::Deserialize;

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
   runtimes:    String,
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
         runtimes:    String::new(),
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

   /// The linker command clang would run for `args`, which also names the
   /// libraries clang adds itself.
   fn linker_command(&self, language: Language, args: &[String]) -> Option<Vec<String>> {
      let printed = Command::new(self.compiler(language))
         .arg("-###")
         .args(args)
         .output()
         .ok()?;
      let text = String::from_utf8_lossy(&printed.stderr);
      let line = text.lines().rev().find(|line| line.starts_with(" \""))?;
      let mut words = Vec::new();
      let mut word = String::new();
      let mut quoted = false;
      let mut characters = line.chars();
      while let Some(character) = characters.next() {
         match character {
            '"' if quoted => {
               words.push(mem::take(&mut word));
               quoted = false;
            },
            '"' => quoted = true,
            '\\' if quoted => word.extend(characters.next()),
            _ if quoted => word.push(character),
            _ => {},
         }
      }
      Some(words)
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
      clippy::cognitive_complexity,
      reason = "compiler mode rewrite is linear, each case is in one place"
   )]
   #[expect(
      clippy::too_many_lines,
      reason = "compiler mode rewrite tracks arguments and runpath in one pass"
   )]
   pub fn arguments(&self, language: Language, raw: &[String]) -> Vec<String> {
      if !self.configured {
         return raw.to_vec();
      }
      let cxx = language == Language::Cxx;
      let fortran = language == Language::Fortran;
      let mut user = Vec::new();
      let mut runpath = Runpath::default();
      let mut linking = true;
      let mut have_input = false;
      let mut shared = false;
      let mut no_policy = false;
      let mut optimizes = true;
      let mut sets_fortify = false;
      let mut index = 0;
      while index < raw.len() {
         if self.format == BinaryFormat::Elf
            && let Some((value, consumed)) = take_rpath(&raw[index..])
         {
            for part in value.split(':').filter(|part| !host_directory(part)) {
               runpath.verbatim.push_str(part);
               runpath.verbatim.push(':');
            }
            index += consumed;
            continue;
         }
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

      let link_args = self.linker_command(language, &output).unwrap_or_else(|| {
         let mut given = user;
         given.extend_from_slice(&self.package.ldflags);
         given
      });
      let mut directories = Vec::new();
      let mut libraries = Vec::new();
      let mut runtime = cxx;
      for (position, arg) in link_args.iter().enumerate() {
         let value = |flag: &str| {
            let tail = arg.strip_prefix(flag)?;
            if tail.is_empty() {
               link_args.get(position + 1).map(String::as_str)
            } else {
               Some(tail)
            }
         };
         if let Some(directory) = value("-L") {
            directories.push(directory);
         } else if let Some(library) = value("-l") {
            runtime |= matches!(
               library,
               "c++" | "stdc++" | "gcc_s" | "unwind" | ":libunwind.so"
            );
            libraries.push(library);
         } else if is_store_path(arg)
            && Path::new(arg)
               .file_name()
               .is_some_and(|name| shared_library(&name.to_string_lossy()))
         {
            let real = canonicalize(arg).unwrap_or_else(|_| Path::new(arg).to_path_buf());
            if let Some(parent) = real.parent() {
               runpath.add(parent.to_string_lossy().into_owned());
            }
         }
         if matches!(arg.as_str(), "-nostdlib++" | "-nostdlib") {
            runtime = false;
         }
      }
      for directory in directories {
         let real = canonicalize(directory).unwrap_or_else(|_| Path::new(directory).to_path_buf());
         if is_store_path(&real.to_string_lossy())
            && libraries.iter().any(|library| {
               real.join(format!("lib{library}.so")).exists()
                  || real.join(format!("lib{library}.dylib")).exists()
            })
         {
            runpath.add(real.to_string_lossy().into_owned());
         }
      }
      if runtime && !self.runtimes.is_empty() {
         runpath.add(self.runtimes.clone());
      }
      runpath.add(format!("{}/lib", self.libc));
      let entries = runpath.entries.len() + runpath.verbatim.matches(':').count();
      let mut path = runpath.verbatim;
      for directory in &runpath.entries {
         path.push_str(directory);
         path.push_str("/.:");
      }
      // rounded up so a small change in what the link names leaves every
      // output's bytes as they were
      let slack = (entries.max(1) * RUNPATH_SLACK + libraries.len() * 80)
         .next_power_of_two()
         .max(1024);
      path.push('/');
      path.push_str(&"_".repeat(slack - 1));
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

#[derive(Default)]
struct Runpath {
   entries:  Vec<String>,
   verbatim: String,
}

impl Runpath {
   fn add(&mut self, directory: String) {
      if !host_directory(&directory) && !self.entries.contains(&directory) {
         self.entries.push(directory);
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

fn take_rpath(args: &[String]) -> Option<(&str, usize)> {
   let first = args.first()?;
   if matches!(first.as_str(), "-Wl,-rpath" | "-Wl,--rpath") {
      return args.get(1)?.strip_prefix("-Wl,").map(|value| (value, 2));
   }
   for spelling in ["-Wl,-rpath,", "-Wl,--rpath,", "-Wl,-rpath=", "-Wl,--rpath="] {
      if let Some(value) = first.strip_prefix(spelling) {
         return Some((value, 1));
      }
   }
   if let [ref linker, ref flag, ref second, ref value, ..] = *args
      && linker == "-Xlinker"
      && matches!(flag.as_str(), "-rpath" | "--rpath")
      && second == "-Xlinker"
   {
      return Some((value, 4));
   }
   None
}
