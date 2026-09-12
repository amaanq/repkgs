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
   mem::take,
   path::Path,
};

use serde::Deserialize;

use crate::store::is_store_path;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum BinaryFormat {
   Elf,
   MachO,
   Coff,
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

pub struct Driver {
   compiler:    String,
   format:      BinaryFormat,
   configured:  bool,
   libc:        String,
   interpreter: String,
   crt:         String,
   runtimes:    String,
   flags:       Vec<String>,
   cxxflags:    Vec<String>,
   prefix_map:  Vec<String>,
   package:     PackageFlags,
}

impl Driver {
   pub fn compiler(&self) -> &str {
      &self.compiler
   }
   pub const fn format(&self) -> BinaryFormat {
      self.format
   }
   pub fn load() -> io::Result<Self> {
      let executable = current_exe()?;
      let root = executable
         .parent()
         .and_then(Path::parent)
         .ok_or_else(|| io::Error::other("jig has no installation root"))?;
      let mut driver = Self {
         compiler:    String::new(),
         format:      BinaryFormat::Elf,
         configured:  false,
         libc:        String::new(),
         interpreter: "ld-linux-x86-64.so.2".to_owned(),
         crt:         String::new(),
         runtimes:    String::new(),
         flags:       Vec::new(),
         cxxflags:    Vec::new(),
         prefix_map:  Vec::new(),
         package:     PackageFlags::default(),
      };
      match read_to_string(root.join("etc/jig.conf")) {
         Ok(text) => {
            driver.configured = true;
            for line in text
               .lines()
               .map(str::trim)
               .filter(|line| !line.is_empty() && !line.starts_with('#'))
            {
               let (key, value) = line
                  .split_once('=')
                  .ok_or_else(|| io::Error::other("invalid jig.conf entry"))?;
               let setting = value.trim();
               match key.trim() {
                  "cc" => setting.clone_into(&mut driver.compiler),
                  "binfmt" => {
                     driver.format = match setting {
                        "elf" => BinaryFormat::Elf,
                        "macho" => BinaryFormat::MachO,
                        "coff" => BinaryFormat::Coff,
                        _ => return Err(io::Error::other("unknown jig binary format")),
                     }
                  },
                  "libc" => setting.clone_into(&mut driver.libc),
                  "interp" => setting.clone_into(&mut driver.interpreter),
                  "crt" => setting.clone_into(&mut driver.crt),
                  "runtimes" => setting.clone_into(&mut driver.runtimes),
                  "flags" => driver.flags = setting.split_whitespace().map(str::to_owned).collect(),
                  "cxxflags" => {
                     driver.cxxflags = setting.split_whitespace().map(str::to_owned).collect();
                  },
                  "prefix-map" => {
                     driver.prefix_map = setting
                        .split(':')
                        .filter(|part| !part.is_empty())
                        .map(str::to_owned)
                        .collect();
                  },
                  _ => {
                     return Err(io::Error::other(format!(
                        "unknown jig.conf key {}",
                        key.trim()
                     )));
                  },
               }
            }
            if let Ok(text) = var("PKGS_CC") {
               let mut packages: HashMap<String, PackageFlags> = serde_json::from_str(&text)?;
               let name = root
                  .to_str()
                  .ok_or_else(|| io::Error::other("jig installation path is not UTF-8"))?;
               driver.package = packages.remove(name).unwrap_or_default();
            }
         },
         Err(error) if error.kind() == io::ErrorKind::NotFound => {
            driver.compiler = var("JIG_CC").unwrap_or_default();
         },
         Err(error) => return Err(error),
      }
      if driver.compiler.is_empty() {
         return Err(io::Error::other("jig needs etc/jig.conf or JIG_CC"));
      }
      Ok(driver)
   }

   pub fn arguments(&self, cxx: bool, raw: &[String]) -> Vec<String> {
      if !self.configured {
         return raw.to_vec();
      }
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
         if let Some((value, consumed)) = take_rpath(&raw[index..]) {
            for part in value.split(':').filter(|part| !host_directory(part)) {
               runpath.verbatim.push_str(part);
               runpath.verbatim.push(':');
            }
            index += consumed;
            continue;
         }
         let arg = &raw[index];
         index += 1;
         if arg.starts_with("-Wl,--dynamic-linker") || arg.starts_with("-Wl,-dynamic-linker") {
            continue;
         }
         linking &= !matches!(
            arg.as_str(),
            "-c" | "-E" | "-S" | "-M" | "-MM" | "-fsyntax-only" | "-###"
         );
         no_policy |= matches!(
            arg.as_str(),
            "-static" | "-static-pie" | "-r" | "-Wl,-r" | "-nostartfiles"
         );
         shared |= arg == "-shared";
         have_input |= !arg.starts_with('-');
         sets_fortify |= fortify(arg);
         if arg.starts_with("-O") {
            optimizes = arg != "-O0";
         }
         if self.format == BinaryFormat::Coff
            && (arg.starts_with("-std=c") || arg.starts_with("-std=gnu"))
            && ["++98", "++03", "++0x", "++11"]
               .iter()
               .any(|suffix| arg.ends_with(suffix))
         {
            user.push(format!("{}14", &arg[..arg.len() - 2]));
         } else {
            user.push(arg.to_owned());
         }
      }
      let mut output = vec!["--start-no-unused-arguments".to_owned()];
      output.extend_from_slice(&self.flags);
      output.extend(
         self
            .package
            .cflags
            .iter()
            .filter(|flag| !fortify(flag) || (!sets_fortify && optimizes))
            .cloned(),
      );
      if cxx {
         output.push("--driver-mode=g++".to_owned());
         output.extend_from_slice(&self.cxxflags);
         output.extend_from_slice(&self.package.cxxflags);
      }
      for mapping in self.prefix_map.iter().map(String::as_str).chain(
         var("PKGS_PREFIX_MAP")
            .unwrap_or_default()
            .split(':')
            .filter(|part| !part.is_empty()),
      ) {
         output.push(format!("-ffile-prefix-map={mapping}"));
      }
      output.push("--end-no-unused-arguments".to_owned());
      output.extend_from_slice(&user);
      if linking && have_input {
         output.extend_from_slice(&self.package.ldflags);
      }
      if !linking || !have_input || no_policy || self.format != BinaryFormat::Elf {
         return output;
      }

      let mut link_args = user;
      link_args.extend_from_slice(&self.package.ldflags);
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
            && libraries
               .iter()
               .any(|library| real.join(format!("lib{library}.so")).exists())
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
      let slack = entries.max(1) * 48 + libraries.len() * 80;
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
   if let [linker, flag, second, value, ..] = args
      && linker == "-Xlinker"
      && matches!(flag.as_str(), "-rpath" | "--rpath")
      && second == "-Xlinker"
   {
      return Some((value, 4));
   }
   None
}

pub fn shared_library(name: &str) -> bool {
   name.rsplit_once(".so").is_some_and(|(_, tail)| {
      tail.is_empty()
         || tail.strip_prefix('.').is_some_and(|suffix| {
            suffix
               .bytes()
               .all(|byte| byte == b'.' || byte.is_ascii_digit())
         })
   })
}

pub fn expand_response_files(args: &[String]) -> io::Result<Vec<String>> {
   let mut output = Vec::new();
   for arg in args {
      let Some(path) = arg.strip_prefix('@') else {
         output.push(arg.to_owned());
         continue;
      };
      let text = match read_to_string(path) {
         Ok(text) => text,
         Err(error) if error.kind() == io::ErrorKind::NotFound => {
            output.push(arg.to_owned());
            continue;
         },
         Err(error) => return Err(error),
      };
      let mut word = String::new();
      let mut quoted = None;
      let mut started = false;
      let mut characters = text.chars();
      while let Some(character) = characters.next() {
         if character == '\\' {
            word.push(characters.next().unwrap_or('\\'));
            started = true;
         } else if quoted == Some(character) {
            quoted = None;
         } else if quoted.is_none() && matches!(character, '\'' | '"') {
            quoted = Some(character);
            started = true;
         } else if quoted.is_none() && character.is_whitespace() {
            if started {
               output.push(take(&mut word));
            }
            started = false;
         } else {
            word.push(character);
            started = true;
         }
      }
      if quoted.is_some() {
         return Err(io::Error::other(
            "unterminated quote in compiler response file",
         ));
      }
      if started {
         output.push(word);
      }
   }
   Ok(output)
}
