use std::{
   env::{
      current_dir,
      current_exe,
      temp_dir,
      var,
   },
   ffi::OsStr,
   fs::{
      canonicalize,
      read,
      read_dir,
      read_to_string,
   },
   io::{
      self,
      Write as _,
   },
   path::{
      Path,
      PathBuf,
   },
   time::Instant,
};

use blake3::Hasher;
use mummer::{
   files::write_file,
   process::{
      Slot,
      diagnostic,
      expand_response_files,
      log,
      log_uncached,
      run,
      run_slotted,
   },
   store::{
      Store,
      field,
      on_path,
   },
};
use tempfile::Builder;
use tightrope::client::Client;

use crate::{
   driver::{
      BinaryFormat,
      Driver,
      Language,
   },
   manifest::{
      Manifest,
      Miss,
      lookup,
      parse_depfile,
      prefetch,
   },
};

#[derive(Clone, Copy, PartialEq)]
enum Action {
   Compile,
   Precompile,
   Assembly,
   Preprocess,
   LinkSource,
   Link,
   Other,
}

struct Invocation {
   args:      Vec<String>,
   key_args:  Vec<String>,
   source:    String,
   target:    String,
   inputs:    Vec<String>,
   output:    PathBuf,
   depfile:   Option<PathBuf>,
   action:    Action,
   query:     bool,
   cacheable: bool,
   stdout:    bool,
}

impl Invocation {
   #[expect(
      clippy::too_many_lines,
      reason = "single-pass option scanner, splitting would scatter invocation state"
   )]
   fn parse(args: Vec<String>) -> Self {
      let mut invocation = Self {
         args,
         key_args: Vec::new(),
         source: String::new(),
         target: String::new(),
         inputs: Vec::new(),
         output: PathBuf::new(),
         depfile: None,
         action: Action::Link,
         query: false,
         cacheable: true,
         stdout: false,
      };
      let mut sources = 0_i32;
      let mut header = false;
      let mut emit_pch = false;
      let mut wants_deps = false;
      let mut objects = false;
      let mut index = 0;
      while index < invocation.args.len() {
         let arg = invocation.args[index].clone();
         let next = invocation.args.get(index + 1).cloned();
         let mut keyed = true;
         let mut consumed = false;
         match arg.as_str() {
            "-c" => invocation.action = Action::Compile,
            "-S" => invocation.action = Action::Assembly,
            "-E" => invocation.action = Action::Preprocess,
            "-o" => {
               keyed = false;
               if let Some(ref value) = next {
                  invocation.output = PathBuf::from(value);
                  consumed = true;
               }
            },
            "-MF" => {
               keyed = false;
               if let Some(ref value) = next {
                  invocation.depfile = Some(PathBuf::from(value));
                  wants_deps = true;
                  consumed = true;
               }
            },
            "-MT" | "-MQ" => {
               keyed = false;
               if let Some(ref value) = next {
                  value.clone_into(&mut invocation.target);
                  wants_deps = true;
                  consumed = true;
               }
            },
            "-MD" | "-MMD" | "-MP" => {
               keyed = false;
               wants_deps = true;
            },
            "-M" | "-MM" | "-" | "-v" | "-V" | "--version" | "-###" | "-fsyntax-only" => {
               invocation.query = true;
            },
            "-shared" | "-r" => objects = true,
            "-I" | "-isystem" | "-iquote" | "-idirafter" | "-include" | "-imacros"
            | "-isysroot" | "--sysroot" | "-L" | "-l" | "-x" | "-D" | "-U" | "-target"
            | "--target" => {
               if let Some(ref value) = next {
                  header |= arg == "-x" && value.ends_with("-header");
                  consumed = true;
               }
            },
            _ => {
               if arg.starts_with("-o") && arg.len() > 2 {
                  keyed = false;
                  invocation.output = PathBuf::from(&arg[2..]);
               } else if arg.starts_with("-Wp,-MD,") || arg.starts_with("-Wp,-MMD,") {
                  keyed = false;
                  invocation.depfile = arg.splitn(3, ',').nth(2).map(PathBuf::from);
                  wants_deps = true;
               } else if !arg.starts_with('-')
                  && [
                     ".c", ".cc", ".cpp", ".cxx", ".c++", ".C", ".S", ".s", ".sx", ".m",
                  ]
                  .iter()
                  .any(|suffix| arg.ends_with(suffix))
               {
                  keyed = false;
                  invocation.source.clone_from(&arg);
                  sources += 1_i32;
               } else if !arg.starts_with('-')
                  && ([".o", ".os", ".lo", ".a", ".so", ".obj"]
                     .iter()
                     .any(|suffix| arg.ends_with(suffix))
                     || arg.contains(".so."))
               {
                  invocation.inputs.push(arg.clone());
                  objects = true;
               }
               emit_pch |= arg == "-emit-pch";
               header |= arg.ends_with("-header");
               invocation.query |= arg.starts_with("-print")
                  || arg.starts_with("--print")
                  || arg.starts_with("-dump");
               invocation.cacheable &= !arg.starts_with('@')
                  && !arg.contains(",@")
                  && ![
                     "-Map",
                     "--print-map",
                     "--dependency-file",
                     "--out-implib",
                     "--trace",
                     "--verbose",
                     "--print-gc-sections",
                     "--stats",
                     "-save-temps",
                     "-ftime-trace",
                     "-fprofile",
                     "--coverage",
                     "-fmodules",
                     "-MJ",
                     "-serialize-diagnostics",
                     "-gsplit-dwarf",
                  ]
                  .iter()
                  .any(|marker| arg.contains(marker))
                  && !arg.ends_with(",-M")
                  && !arg.ends_with(",-t");
            },
         }
         if keyed {
            invocation.key_args.push(arg);
            if consumed && let Some(value) = next {
               invocation.key_args.push(value);
            }
         }
         index += if consumed { 2 } else { 1 };
      }
      if matches!(
         invocation.action,
         Action::Compile | Action::Precompile | Action::Assembly | Action::Preprocess
      ) {
         invocation.cacheable &= sources == 1_i32;
         invocation.query |= invocation.action == Action::Preprocess && sources == 0_i32;
         if invocation.action == Action::Preprocess
            && (invocation.output.as_os_str().is_empty() || invocation.output == Path::new("-"))
         {
            invocation.stdout = true;
         } else if invocation.output.as_os_str().is_empty() {
            invocation.output = Path::new(&invocation.source)
               .file_stem()
               .map_or_else(|| PathBuf::from("output"), PathBuf::from);
            invocation
               .output
               .set_extension(if invocation.action == Action::Assembly {
                  "s"
               } else {
                  "o"
               });
         }
      } else {
         invocation.action = match (objects, sources) {
            (false, 1_i32) => Action::LinkSource,
            (true, 0_i32) if !invocation.inputs.is_empty() => Action::Link,
            _ => Action::Other,
         };
         if invocation.output.as_os_str().is_empty() {
            invocation.output = PathBuf::from("a.out");
         }
      }
      let extension = invocation.output.extension();
      if emit_pch
         && header
         && invocation.action == Action::Compile
         && extension.is_some_and(|found| found == "pch")
      {
         invocation.action = Action::Precompile;
      }
      let precompile = invocation.action == Action::Precompile;
      invocation.cacheable &= !invocation.query
         && invocation.output != Path::new("/dev/null")
         && invocation.action != Action::Other
         && (!header || precompile)
         && (precompile || !extension.is_some_and(|found| found == "pch" || found == "gch"));
      invocation.read_precompiled();
      if wants_deps && invocation.depfile.is_none() {
         invocation.depfile = Some(invocation.output.with_extension("d"));
      }
      invocation
   }

   /// Every `-include-pch` file is an input. The depfile lists the headers
   /// inside it but not the .pch itself.
   fn read_precompiled(&mut self) {
      for (position, arg) in self.args.iter().enumerate() {
         if arg != "-include-pch" {
            continue;
         }
         let after = if self
            .args
            .get(position + 1)
            .is_some_and(|next| next == "-Xclang")
         {
            position + 2
         } else {
            position + 1
         };
         match self.args.get(after) {
            Some(pch) => self.inputs.push(pch.clone()),
            None => self.cacheable = false,
         }
      }
   }

   /// clang reopens the headers a .pch names, so a precompile matches them
   /// exactly.
   fn manifest_store(&mut self) -> Store {
      if self.action != Action::Precompile {
         return Store::new();
      }
      self
         .args
         .extend(["-Xclang".to_owned(), "-fno-pch-timestamp".to_owned()]);
      Store::exact()
   }

   const fn links(&self) -> bool {
      matches!(self.action, Action::Link | Action::LinkSource)
   }

   fn key_injected(&mut self, user: &[String]) {
      for arg in &self.args {
         if !user.contains(arg) {
            self.key_args.push(arg.clone());
         }
      }
   }

   fn retarget(&self, depfile: &str) -> String {
      let target = if self.target.is_empty() {
         self.output.to_string_lossy().into_owned()
      } else {
         self.target.clone()
      };
      match depfile.find(": ") {
         Some(colon) if depfile.find('\n').is_none_or(|newline| newline > colon) => {
            format!("{target}{}", &depfile[colon..])
         },
         _ => depfile.to_owned(),
      }
   }

   #[expect(
      clippy::filetype_is_file,
      reason = "toolchain identities include regular library files only"
   )]
   fn toolchain_identities(
      &self,
      cache: &mut Client,
      store: &mut Store,
      compiler: &str,
   ) -> io::Result<String> {
      let mut tools = vec![on_path(compiler).to_string_lossy().into_owned()];
      if self.links() {
         tools.extend(
            self
               .args
               .iter()
               .filter_map(|arg| arg.strip_prefix("--ld-path="))
               .map(|value| on_path(value).to_string_lossy().into_owned()),
         );
      }
      let mut files = Vec::new();
      for tool in &tools {
         let real = canonicalize(tool)?;
         files.push(real.to_string_lossy().into_owned());
         let library = real
            .parent()
            .and_then(Path::parent)
            .map(|root| root.join("lib"));
         let Some(directory) = library else {
            continue;
         };
         let entries = match read_dir(directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
         };
         for directory_entry in entries {
            let entry = directory_entry?;
            let name = entry.file_name();
            let text = name.to_string_lossy();
            let shared = text.starts_with("libLLVM")
               || text.starts_with("libclang-cpp")
               || text.starts_with("liblld");
            if shared && entry.file_type()?.is_file() {
               files.push(entry.path().to_string_lossy().into_owned());
            }
         }
      }
      files.sort_unstable();
      files.dedup();
      prefetch(cache, store, &files)?;
      let mut identities = String::new();
      for file in &files {
         identities.push_str(&store.input_id(file)?);
         identities.push(',');
      }
      Ok(identities)
   }

   fn request_key(
      &self,
      store: &mut Store,
      cache: &mut Client,
      compiler: &str,
   ) -> io::Result<String> {
      let mut hasher = Hasher::new();
      field(&mut hasher, store.tool_id(compiler)?);
      field(
         &mut hasher,
         self.toolchain_identities(cache, store, compiler)?,
      );
      field(&mut hasher, store.key(&current_dir()?.to_string_lossy()));
      field(&mut hasher, match self.action {
         Action::Link => "link",
         Action::LinkSource => "link-one",
         Action::Precompile => "precompile",
         Action::Compile | Action::Assembly | Action::Preprocess | Action::Other => "compile",
      });
      // the toolchain's own paths move with every mummer build
      let toolchain = current_exe()
         .ok()
         .and_then(|exe| Some(exe.parent()?.parent()?.to_string_lossy().into_owned()));
      for argument in &self.key_args {
         let mut bytes = argument.as_bytes().to_vec();
         store.mask_output(&mut bytes, false);
         let masked = String::from_utf8_lossy(&bytes);
         let embedded =
            self.links() && (argument.contains("dynamic-linker") || argument.contains("rpath"));
         field(&mut hasher, match toolchain {
            Some(ref root) if self.action == Action::Precompile => {
               masked.replace(root.as_str(), "<toolchain>")
            },
            _ if embedded => masked.into_owned(),
            _ => store.key(&masked),
         });
      }
      for name in [
         "CPATH",
         "C_INCLUDE_PATH",
         "CPLUS_INCLUDE_PATH",
         "OBJC_INCLUDE_PATH",
         "LIBRARY_PATH",
         "SOURCE_DATE_EPOCH",
         "LANG",
         "LC_ALL",
      ] {
         field(&mut hasher, name);
         field(&mut hasher, var(name).unwrap_or_default());
      }
      if self.action != Action::Link {
         field(&mut hasher, "source");
         field(&mut hasher, self.source.as_bytes());
         let mut bytes = read(&self.source)?;
         store.mask_output(&mut bytes, false);
         field(&mut hasher, bytes);
      }
      for path in &self.inputs {
         field(&mut hasher, store.input_id(path)?);
      }
      Ok(format!("cc/{}", &hasher.finalize().to_hex()[..32]))
   }
}

pub fn cached_header(driver: &Driver, raw: &[String], cache: &mut Client) -> io::Result<bool> {
   let mut invocation = Invocation::parse(raw.to_vec());
   if invocation.action != Action::Compile || !invocation.cacheable {
      return Ok(false);
   }
   invocation.args = driver.arguments(Language::Cc, raw);
   invocation.key_injected(raw);
   let mut store = Store::new();
   let request = invocation.request_key(&mut store, cache, driver.compiler(Language::Cc))?;
   let key = match lookup(cache, &mut store, &request) {
      Ok(key) => key,
      Err(Miss::Failed(error)) => return Err(error),
      Err(_) => return Ok(false),
   };
   let results = cache.get_many(&[format!("r/{key}"), format!("e/{key}")])?;
   Ok(
      matches!(results.as_slice(), [Some(status), Some(diagnostics)]
      if status == b"0" && diagnostics.is_empty()),
   )
}

#[expect(
   clippy::too_many_lines,
   reason = "compile-cache pipeline from lookup to store, stages read linearly"
)]
pub fn run_cc(mode: &str, raw: &[String], socket: &Path) -> io::Result<i32> {
   let started = Instant::now();
   let user = expand_response_files(raw)?;
   let driver = Driver::load()?;
   let language = if mode.contains("++") {
      Language::Cxx
   } else if mode.contains("fortran") || mode.contains("flang") {
      Language::Fortran
   } else {
      Language::Cc
   };
   let compiler = driver.compiler(language);

   if compiler.is_empty() {
      return Err(io::Error::other(format!(
         "{mode} called but etc/mummer.json names no fc"
      )));
   }

   let mut invocation = Invocation::parse(user.clone());
   if (invocation.links() && driver.format() != BinaryFormat::Elf) || language == Language::Fortran
   {
      invocation.cacheable = false;
   }
   invocation.args = driver.arguments(language, &invocation.args);
   let mut store = Store::new();
   let mut manifest_store = invocation.manifest_store();
   invocation.key_injected(&user);
   let subject = if invocation.source.is_empty() {
      invocation.output.to_string_lossy().into_owned()
   } else {
      invocation.source.clone()
   };
   let connected = if invocation.cacheable {
      match Client::connect(socket).and_then(|mut cache| {
         invocation
            .request_key(&mut store, &mut cache, compiler)
            .map(|key| (key, cache))
      }) {
         Ok(connection) => Some(connection),
         Err(error)
            if matches!(
               error.kind(),
               io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
            ) =>
         {
            None
         },
         Err(error) => {
            diagnostic(format_args!(
               "mummer compilation cache unavailable, {error}"
            ));
            None
         },
      }
   } else {
      None
   };
   let Some((request, mut cache)) = connected else {
      let slot_socket = (!invocation.query).then_some(socket);
      let result = run_slotted(OsStr::new(compiler), &invocation.args, slot_socket, false)?;
      let outcome = if invocation.query {
         "query"
      } else if !invocation.cacheable {
         if invocation.links() || invocation.action == Action::Other {
            "linked"
         } else {
            "uncacheable"
         }
      } else {
         "no-daemon"
      };
      log("cc", outcome, &subject, started);
      log_uncached(outcome, &user);
      return Ok(result.status);
   };
   let hit = lookup(&mut cache, &mut manifest_store, &request)
      .and_then(|key| cached(&mut cache, &store, &invocation, &key));
   let miss = match hit {
      Ok(status) => {
         log(
            "cc",
            if status == 0_i32 {
               "cached"
            } else {
               "cached-error"
            },
            &subject,
            started,
         );
         return Ok(status);
      },
      Err(Miss::Failed(error)) => {
         diagnostic(format_args!(
            "mummer cache replay failed, compiling again, {error}"
         ));
         Miss::Failed(error)
      },
      Err(miss) => miss,
   };
   let explained = format!("{subject} {miss}");

   let scratch = temp_dir();
   let parent = if invocation.stdout {
      scratch.as_path()
   } else {
      invocation
         .output
         .parent()
         .filter(|path| !path.as_os_str().is_empty())
         .unwrap_or_else(|| Path::new("."))
   };
   let dependency = Builder::new()
      .prefix(".mummer-")
      .suffix(".d")
      .tempfile_in(parent)?;
   let link_dependency = Builder::new()
      .prefix(".mummer-")
      .suffix(".link.d")
      .tempfile_in(parent)?;
   let absent_log = Builder::new()
      .prefix(".mummer-")
      .suffix(".absent")
      .tempfile_in(parent)?;
   let output_capture = if invocation.stdout {
      Some(Builder::new().prefix(".mummer-").suffix(".i").tempfile()?)
   } else {
      None
   };
   let output_path = if let Some(ref file) = output_capture {
      file.path()
   } else {
      invocation.output.as_path()
   };
   let dependency_path = invocation
      .depfile
      .as_deref()
      .unwrap_or_else(|| dependency.path());
   let mut arguments = invocation.args.clone();
   let own_depfile = invocation.depfile.is_none() && invocation.action != Action::Link;
   let plain_assembly = Path::new(&invocation.source)
      .extension()
      .is_some_and(|extension| extension == "s");

   if own_depfile && !plain_assembly {
      arguments.extend([
         "-MD".to_owned(),
         "-MF".to_owned(),
         dependency_path.to_string_lossy().into_owned(),
      ]);
   } else if !own_depfile
      && invocation.action != Action::Link
      && ![".S", ".s", ".sx"]
         .iter()
         .any(|suffix| invocation.source.ends_with(suffix))
   {
      arguments.extend(["-Xclang".to_owned(), "-sys-header-deps".to_owned()]);
   }
   if invocation.stdout {
      arguments.extend(["-o".to_owned(), output_path.to_string_lossy().into_owned()]);
   }
   if invocation.links() {
      arguments.push(format!(
         "-Wl,--dependency-file={}",
         link_dependency.path().display()
      ));
   }
   let result = run(
      OsStr::new(compiler),
      &arguments,
      Slot::acquire(&mut cache).ok().flatten(),
      true,
      &[("MUMMER_ABSENT_LOG", absent_log.path().as_os_str())],
   )?;
   io::stderr().lock().write_all(&result.stderr)?;
   if invocation.stdout {
      io::stdout().lock().write_all(&read(output_path)?)?;
   }
   let stored = (|| -> io::Result<bool> {
      let dependencies = if invocation.action == Action::Link {
         String::new()
      } else if own_depfile && plain_assembly {
         format!("o: {}", invocation.source)
      } else {
         read_to_string(dependency_path)?
      };
      let mut inputs = parse_depfile(&dependencies);
      if invocation.links() {
         let link_text = read_to_string(link_dependency.path())?;
         if link_text.is_empty() {
            return Ok(false);
         }
         let temporary = var("TMPDIR").unwrap_or_else(|_| "/tmp".to_owned());
         let prefix = format!("{}/", temporary.trim_end_matches('/'));
         inputs.extend(parse_depfile(&link_text).into_iter().filter(|path| {
            if !path.starts_with(&prefix) {
               return true;
            }
            path[prefix.len()..].contains('/')
         }));
      }
      if result.status != 0_i32
         && (invocation.links()
            || inputs.is_empty()
            || String::from_utf8_lossy(&result.stderr).contains("file not found")
            || !result.deterministic())
      {
         return Ok(false);
      }
      if !invocation.source.is_empty() {
         inputs.push(invocation.source.clone());
      }
      inputs.extend_from_slice(&invocation.inputs);
      inputs.sort_unstable();
      inputs.dedup();
      let absent_text = read_to_string(absent_log.path())?;
      let absent = absent_text.lines().map(str::to_owned).collect::<Vec<_>>();
      let manifest = Manifest::build(&mut cache, &mut manifest_store, &inputs, &[], &absent)?;
      let key = manifest.result_key(&request)?;
      if result.status == 0_i32 {
         let mut object = read(output_path)?;
         store.mask_output(&mut object, false);
         cache.put(&format!("o/{key}"), &object)?;
         if invocation.depfile.is_some() {
            let mut dep_bytes = store.mask_for_replay(&dependencies).into_bytes();
            store.mask_output(&mut dep_bytes, false);
            cache.put(&format!("d/{key}"), &dep_bytes)?;
         }
      }
      let mut diagnostics = result.stderr.clone();
      store.mask_output(&mut diagnostics, false);
      cache.put(&format!("e/{key}"), &diagnostics)?;
      cache.put(&format!("r/{key}"), result.status.to_string().as_bytes())?;
      manifest.publish(&mut cache, &request)?;
      Ok(true)
   })();
   let outcome = match stored {
      Ok(true) => {
         if result.status == 0_i32 {
            "compiled"
         } else {
            "compiled-error"
         }
      },
      Ok(false) => {
         if result.status == 0_i32 {
            "compiled-unstored"
         } else {
            "failed"
         }
      },
      Err(error) => {
         diagnostic(format_args!(
            "mummer could not store compiler result, {error}"
         ));
         if result.status == 0_i32 {
            "compiled-unstored"
         } else {
            "failed"
         }
      },
   };
   log("cc", outcome, &explained, started);
   Ok(result.status)
}

fn cached(
   cache: &mut Client,
   store: &Store,
   invocation: &Invocation,
   key: &str,
) -> Result<i32, Miss> {
   let keys = [
      format!("r/{key}"),
      format!("o/{key}"),
      format!("d/{key}"),
      format!("e/{key}"),
   ];
   let mut objects = cache.get_many(&keys)?.into_iter();
   let Some(Some(status_bytes)) = objects.next() else {
      return Err(Miss::ObjectGone);
   };
   let status = String::from_utf8(status_bytes)
      .map_err(io::Error::other)?
      .parse::<u8>()
      .map_err(io::Error::other)?;
   let object = objects.next().flatten();
   let dependency = objects.next().flatten();
   let Some(Some(mut diagnostics)) = objects.next() else {
      return Err(Miss::ObjectGone);
   };
   if status == 0 {
      let Some(mut bytes) = object else {
         return Err(Miss::ObjectGone);
      };
      let dep_text = if invocation.depfile.is_some() {
         let Some(mut dep_bytes) = dependency else {
            return Err(Miss::ObjectGone);
         };
         store.mask_output(&mut dep_bytes, true);
         let raw = String::from_utf8(dep_bytes).map_err(io::Error::other)?;
         Some(invocation.retarget(&store.resolve_all(&raw)))
      } else {
         None
      };
      store.mask_output(&mut bytes, true);
      if invocation.stdout {
         io::stdout().lock().write_all(&bytes)?;
      } else {
         write_file(
            &invocation.output,
            &bytes,
            invocation.links().then_some(0o755),
         )?;
      }
      if let (Some(path), Some(text)) = (invocation.depfile.as_ref(), dep_text.as_ref()) {
         write_file(path, text.as_bytes(), None)?;
      }
   }
   store.mask_output(&mut diagnostics, true);
   io::stderr().lock().write_all(&diagnostics)?;
   Ok(i32::from(status))
}
