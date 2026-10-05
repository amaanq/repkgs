use std::{
   env::{
      args_os,
      current_dir,
      current_exe,
      temp_dir,
      var,
      var_os,
   },
   ffi::OsStr,
   fs::{
      metadata,
      read,
      read_to_string,
   },
   io::{
      self,
      Write as _,
   },
   mem::take,
   os::unix::fs::PermissionsExt as _,
   path::{
      Path,
      PathBuf,
   },
   process::Command,
   time::Instant,
};

use blake3::Hasher;
use mummer::{
   files::{
      remove_if_exists,
      write_file,
   },
   process::{
      RunResult,
      Slot,
      diagnostic,
      expand_response_files,
      log,
      log_uncached,
      run,
      run_slotted,
      status_code,
   },
   store::{
      Store,
      field,
      mask_random_names,
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
      Settled,
      reserve,
      settle_runpath,
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
   args:         Vec<String>,
   key_args:     Vec<String>,
   source:       String,
   target:       String,
   inputs:       Vec<String>,
   output:       PathBuf,
   depfile:      Option<PathBuf>,
   linkfile:     Option<PathBuf>,
   action:       Action,
   query:        bool,
   cacheable:    bool,
   stdout:       bool,
   preprocessed: Option<String>,
   split:        Vec<usize>,
   jobs:         Vec<Vec<String>>,
}

impl Invocation {
   #[expect(
      clippy::too_many_lines,
      clippy::cognitive_complexity,
      reason = "single-pass option scanner, splitting would scatter invocation state"
   )]
   fn parse(args: Vec<String>, jobs: Vec<Vec<String>>) -> Self {
      let mut invocation = Self {
         args,
         key_args: Vec::new(),
         source: String::new(),
         target: String::new(),
         inputs: Vec::new(),
         output: PathBuf::new(),
         depfile: None,
         linkfile: None,
         action: Action::Link,
         query: false,
         cacheable: true,
         stdout: false,
         preprocessed: None,
         split: Vec::new(),
         jobs,
      };
      let (compile_jobs, link_jobs): (Vec<_>, Vec<_>) = invocation.jobs.iter().partition(|job| {
         job.get(1)
            .is_some_and(|mode| matches!(mode.as_str(), "-cc1" | "-cc1as" | "-fc1"))
      });
      let linked = link_jobs.into_iter().flatten().collect::<Vec<_>>();
      let compiled = compile_jobs
         .into_iter()
         .filter_map(|job| {
            Some((
               job.last()?.clone(),
               job.iter().any(|token| token == "-emit-pch"),
            ))
         })
         .collect::<Vec<_>>();
      let mut sources = 0_i32;
      let mut header = false;
      let mut languages = false;
      let mut positions = Vec::new();
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
            | "--target" | "-Xclang" | "-Xlinker" | "-Xassembler" | "-Xpreprocessor" | "-mllvm" => {
               if next.is_some() {
                  languages |= arg == "-x";
                  consumed = true;
               }
            },
            _ => {
               if arg.starts_with("-o") && arg.len() > 2 {
                  keyed = false;
                  invocation.output = PathBuf::from(&arg[2..]);
               } else if arg.starts_with("-x") && arg.len() > 2 {
                  languages = true;
               } else if arg.starts_with("-Wp,-MD,") || arg.starts_with("-Wp,-MMD,") {
                  keyed = false;
                  invocation.depfile = arg.splitn(3, ',').nth(2).map(PathBuf::from);
                  wants_deps = true;
               } else if let Some(&(_, precompiles)) = compiled.iter().find(|entry| entry.0 == arg)
               {
                  keyed = false;
                  invocation.source.clone_from(&arg);
                  sources += 1_i32;
                  positions.push(index);
                  header |= precompiles;
               } else if !arg.starts_with('-') && {
                  // a value the driver hands the linker after its option, as
                  // `-rpath <dir>` or `-z noexecstack`, is no input file
                  let option = index
                     .checked_sub(1)
                     .map(|before| invocation.args[before].as_str())
                     .filter(|before| before.starts_with('-'));
                  linked.iter().enumerate().any(|(position, token)| {
                     **token == arg
                        && !(position > 0
                           && option.is_some_and(|before| linked[position - 1].as_str() == before))
                  })
               } {
                  invocation.inputs.push(arg.clone());
                  objects = true;
               }
               invocation.query |= arg.starts_with("-print")
                  || arg.starts_with("--print")
                  || arg.starts_with("-dump");
            },
         }
         let passed_on = consumed && (arg.starts_with("-X") || arg == "-mllvm");
         invocation.cacheable &= !(uncacheable_marker(&arg)
            || passed_on && next.as_deref().is_some_and(uncacheable_marker));
         if keyed {
            invocation.key_args.push(arg);
            if consumed && let Some(value) = next {
               invocation.key_args.push(value);
            }
         }
         index += if consumed { 2 } else { 1 };
      }
      invocation.linkfile = given_link_depfile(&invocation.args).map(PathBuf::from);
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
         } else if invocation.output.as_os_str().is_empty() && header {
            invocation.output = PathBuf::from(format!("{}.gch", invocation.source));
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
      if header && invocation.action == Action::Compile {
         invocation.action = Action::Precompile;
      }
      if invocation.action == Action::Other
         && sources > 0_i32
         && !languages
         && !wants_deps
         && invocation.cacheable
         && !invocation.query
         && invocation.linkfile.is_none()
         && invocation.output != Path::new("/dev/null")
      {
         invocation.split = positions;
      }
      let precompile = invocation.action == Action::Precompile;
      invocation.cacheable &= !invocation.query
         && invocation.output != Path::new("/dev/null")
         && invocation.action != Action::Other
         && (invocation.linkfile.is_none() || invocation.links())
         && (precompile || !extension.is_some_and(|found| found == "pch" || found == "gch"));
      invocation.read_precompiled();
      if wants_deps && invocation.depfile.is_none() {
         invocation.depfile = Some(invocation.output.with_extension("d"));
      }
      invocation
   }

   /// Every precompiled header a compile job loads is an input, whether
   /// named by `-include-pch` or found beside an `-include`d header. The
   /// depfile lists the headers inside it but not the file itself.
   fn read_precompiled(&mut self) {
      for job in &self.jobs {
         let mut tokens = job.iter();
         while let Some(token) = tokens.next() {
            if token == "-include-pch"
               && let Some(path) = tokens.next()
               && !self.inputs.contains(path)
            {
               self.inputs.push(path.clone());
            }
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

   /// The cc1 command clang runs for this compile, which also carries what its
   /// config files and environment add, the output's own paths as tokens.
   fn compile_job(&self) -> String {
      let mut job = self
         .jobs
         .iter()
         .map(|tokens| format!("\"{}\"", tokens.join("\" \"")))
         .collect::<Vec<_>>()
         .join("\n");
      let output = self.output.to_string_lossy();
      if !output.is_empty() {
         job = job.replace(output.as_ref(), "<output>");
      }
      if let Some(ref depfile) = self.depfile {
         job = job.replace(depfile.to_string_lossy().as_ref(), "<depfile>");
      }
      // a preprocessed .S hands cc1as a file named <stem>-<6 random>.<ext> in
      // TMPDIR
      let scratch = format!("\"{}/", temp_dir().display());
      let mut masked = String::with_capacity(job.len());
      let mut rest = job.as_str();
      while let Some(start) = rest.find(&scratch) {
         let begin = start + scratch.len();
         masked.push_str(&rest[..begin]);
         let end = rest[begin..]
            .find('"')
            .map_or(rest.len(), |length| begin + length);
         let name = &rest[begin..end];
         match name.rsplit_once('-') {
            Some((stem, tail)) if !name.contains('/') && tail.find('.') == Some(6) => {
               masked.push_str(stem);
               masked.push_str("-<temp>");
               masked.push_str(&tail[6..]);
            },
            _ => masked.push_str(name),
         }
         rest = &rest[end..];
      }
      masked.push_str(rest);
      masked
   }

   const fn links(&self) -> bool {
      matches!(self.action, Action::Link | Action::LinkSource)
   }

   /// Keeps a Fortran compile cacheable only with its prescanned source.
   fn prescan(&mut self, language: Language, compiler: &str) -> io::Result<()> {
      if language != Language::Fortran {
         return Ok(());
      }
      self.preprocessed = if self.cacheable && self.action == Action::Compile {
         prescanned(compiler, &self.args)?
      } else {
         None
      };
      self.cacheable &= self.preprocessed.is_some();
      Ok(())
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

   fn toolchain_identities(
      &self,
      cache: &mut Client,
      store: &Store,
      compiler: &str,
   ) -> io::Result<String> {
      let mut identities = store.tool_identity(cache, compiler)?;
      if self.links() {
         for linker in self
            .args
            .iter()
            .filter_map(|arg| arg.strip_prefix("--ld-path="))
         {
            identities.push_str(&store.tool_identity(cache, linker)?);
         }
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
            _ if embedded => store.key_embedded(&masked),
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
      if matches!(
         self.action,
         Action::Compile | Action::Precompile | Action::Assembly | Action::Preprocess
      ) {
         let mut bytes = self.compile_job().into_bytes();
         store.mask_output(&mut bytes, false);
         let job = String::from_utf8_lossy(&bytes);
         field(&mut hasher, match toolchain {
            Some(ref root) if self.action == Action::Precompile => {
               job.replace(root.as_str(), "<toolchain>")
            },
            _ => store.key(&job),
         });
      }
      if let Some(ref text) = self.preprocessed {
         field(&mut hasher, "prescanned");
         let mut bytes = store.mask_for_replay(text).into_bytes();
         store.mask_output(&mut bytes, false);
         field(&mut hasher, bytes);
      }
      if self.action != Action::Link {
         field(&mut hasher, "source");
         field(&mut hasher, mask_random_names(&self.source));
         let mut bytes = read(&self.source)?;
         store.mask_output(&mut bytes, false);
         field(&mut hasher, bytes);
      }
      prefetch(cache, store, &self.inputs)?;
      for path in &self.inputs {
         field(&mut hasher, store.input_id(path)?);
      }
      Ok(format!("cc/{}", &hasher.finalize().to_hex()[..32]))
   }
}

pub fn cached_header(driver: &Driver, raw: &[String], cache: &mut Client) -> io::Result<bool> {
   let expanded = driver.arguments(Language::Cc, raw);
   let mut invocation = Invocation::parse(
      raw.to_vec(),
      jobs(driver.compiler(Language::Cc), &expanded)?,
   );
   if invocation.action != Action::Compile || !invocation.cacheable {
      return Ok(false);
   }
   invocation.args = expanded;
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
   clippy::cognitive_complexity,
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

   let expanded = driver.arguments(language, &user);
   let mut invocation = Invocation::parse(user.clone(), jobs(compiler, &expanded)?);
   if !invocation.split.is_empty() && driver.format() == BinaryFormat::Elf {
      return run_split(&invocation);
   }
   if invocation.links() && driver.format() != BinaryFormat::Elf {
      invocation.cacheable = false;
   }
   invocation.args = expanded;
   invocation.prescan(language, compiler)?;
   let mut store = Store::new();
   let mut manifest_store = invocation.manifest_store();
   store.learn_random(&invocation.args);
   manifest_store.learn_random(&invocation.args);
   invocation.key_injected(&user);
   let subject = if invocation.source.is_empty() {
      invocation.output.to_string_lossy().into_owned()
   } else {
      invocation.source.clone()
   };
   let mut unkeyed = None;
   let connected = if invocation.cacheable {
      match Client::connect(socket) {
         Ok(mut cache) => {
            match invocation.request_key(&mut store, &mut cache, compiler) {
               Ok(key) => Some((key, cache)),
               Err(error) => {
                  unkeyed = Some(error);
                  None
               },
            }
         },
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
      let result = run_uncached(&invocation, driver.format(), compiler, slot_socket)?;
      let outcome = if invocation.query {
         "query"
      } else if !invocation.cacheable {
         if invocation.links() || invocation.action == Action::Other {
            "linked"
         } else {
            "uncacheable"
         }
      } else if unkeyed.is_some() {
         "unkeyed"
      } else {
         "no-daemon"
      };
      let explained = unkeyed.map_or_else(|| subject.clone(), |error| format!("{subject} {error}"));
      log("cc", outcome, &explained, started);
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
   let self_contained = plain_assembly || invocation.preprocessed.is_some();

   if own_depfile && !self_contained {
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
   let link_file = if invocation.links() {
      link_depfile(&mut arguments, link_dependency.path())
   } else {
      link_dependency.path().to_path_buf()
   };
   let (result, linked) = {
      let mut link = |args: &[String]| {
         run(
            OsStr::new(compiler),
            args,
            Slot::acquire(&mut cache).ok().flatten(),
            true,
            &[("MUMMER_ABSENT_LOG", absent_log.path().as_os_str())],
         )
      };
      if invocation.links() && driver.format() == BinaryFormat::Elf {
         settled_link(&invocation.output, &mut arguments, &link_file, &mut link)?
      } else {
         (link(&arguments)?, None)
      }
   };
   io::stderr().lock().write_all(&result.stderr)?;
   if invocation.stdout {
      io::stdout().lock().write_all(&read(output_path)?)?;
   }
   let stored = (|| -> io::Result<bool> {
      let dependencies = if invocation.action == Action::Link {
         String::new()
      } else if own_depfile && self_contained {
         format!("o: {}", invocation.source)
      } else {
         match read_to_string(dependency_path) {
            Ok(text) => text,
            // clang deletes the dependency file when a header is missing
            Err(error) if error.kind() == io::ErrorKind::NotFound && result.status != 0_i32 => {
               return Ok(false);
            },
            Err(error) => return Err(error),
         }
      };
      let mut inputs = parse_depfile(&dependencies);
      if invocation.links() {
         let link_text = read_to_string(&link_file)?;
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
            || !result.reported_error())
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
         let mut object = match linked {
            Some(bytes) => bytes,
            None => read(output_path)?,
         };
         store.mask_output(&mut object, false);
         cache.put(&format!("o/{key}"), &object)?;
         if invocation.depfile.is_some() {
            let mut dep_bytes = store.mask_text(&dependencies).into_bytes();
            store.mask_output(&mut dep_bytes, false);
            cache.put(&format!("d/{key}"), &dep_bytes)?;
         }
         if invocation.links() {
            let text = read_to_string(&link_file)?;
            let target = format!("{}:", invocation.output.display());
            let body = text
               .strip_prefix(&target)
               .map_or_else(|| text.clone(), |rest| format!("<output>:{rest}"));
            let mut link_bytes = store.mask_text(&body).into_bytes();
            store.mask_output(&mut link_bytes, false);
            cache.put(&format!("l/{key}"), &link_bytes)?;
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

/// The jobs the compiler driver would run for `args`, each as its tokens,
/// or none when it refuses them, which leaves the call uncacheable.
pub fn jobs(compiler: &str, args: &[String]) -> io::Result<Vec<Vec<String>>> {
   let printed = Command::new(compiler).arg("-###").args(args).output()?;
   if !printed.status.success() {
      return Ok(Vec::new());
   }
   Ok(String::from_utf8_lossy(&printed.stderr)
      .lines()
      .filter(|line| line.starts_with(" \""))
      .map(job_tokens)
      .collect())
}

/// One `-###` line's quoted tokens, with clang's backslash escapes undone.
fn job_tokens(line: &str) -> Vec<String> {
   let mut tokens = Vec::new();
   let mut token = String::new();
   let mut quoted = false;
   let mut characters = line.chars();
   while let Some(character) = characters.next() {
      match character {
         '"' if quoted => tokens.push(take(&mut token)),
         '"' => {},
         '\\' if quoted => token.extend(characters.next()),
         other if quoted => token.push(other),
         _ => continue,
      }
      quoted ^= character == '"';
   }
   tokens
}

/// A compile and link in one call, run as this wrapper's compile of each
/// source and its link of their objects, the way clang splits it itself,
/// so each half caches.
fn run_split(invocation: &Invocation) -> io::Result<i32> {
   let wrapper = args_os()
      .next()
      .map(|name| on_path(&name.to_string_lossy()))
      .ok_or_else(|| io::Error::other("mummer has no program name"))?;
   let mut shared = Vec::new();
   let mut skip = false;
   for (index, arg) in invocation.args.iter().enumerate() {
      if skip {
         skip = false;
      } else if arg == "-o" {
         skip = true;
      } else if !(invocation.split.contains(&index) || arg.starts_with("-o") && arg.len() > 2) {
         shared.push(arg.clone());
      }
   }
   let mut objects = Vec::new();
   let mut link = Vec::new();
   let mut status = 0_i32;
   for (index, arg) in invocation.args.iter().enumerate() {
      if !invocation.split.contains(&index) {
         link.push(arg.clone());
         continue;
      }
      let object = format!("{}.{}.o", invocation.output.display(), objects.len());
      objects.push(object.clone());
      link.push(object.clone());
      status = status_code(
         Command::new(&wrapper)
            .args(&shared)
            .args([arg.as_str(), "-c", "-o", &object, "-Qunused-arguments"])
            .status()?,
      );
      if status != 0_i32 {
         break;
      }
   }
   if status == 0_i32 {
      link.push("-Qunused-arguments".to_owned());
      status = status_code(Command::new(&wrapper).args(&link).status()?);
   }
   for object in &objects {
      remove_if_exists(Path::new(object))?;
   }
   Ok(status)
}

/// The source as flang prescans it with every INCLUDE inlined, or None when
/// that fails or a statement uses or defines a module, whose `.mod` files
/// the key cannot see.
fn prescanned(compiler: &str, args: &[String]) -> io::Result<Option<String>> {
   let mut arguments = Vec::with_capacity(args.len() + 3);
   let mut words = args.iter();
   while let Some(arg) = words.next() {
      match arg.as_str() {
         "-c" => {},
         "-o" => {
            words.next();
         },
         _ => arguments.push(arg.as_str()),
      }
   }
   arguments.extend(["-E", "-Xflang", "-fpreprocess-include-lines"]);
   let output = Command::new(compiler).args(&arguments).output()?;
   if !output.status.success() {
      return Ok(None);
   }
   let text = String::from_utf8_lossy(&output.stdout).into_owned();
   // fixed form ignores blanks, so "U S E M" is a USE statement and only an =
   // tells USEEI = .TRUE. apart from one
   let modular = text
      .lines()
      .filter(|line| !line.starts_with('#'))
      .flat_map(|line| line.split(';'))
      .map(|statement| {
         statement
            .chars()
            .filter(|character| !character.is_whitespace())
            .collect::<String>()
            .to_ascii_lowercase()
      })
      .any(|statement| {
         let body = statement.trim_start_matches(|character: char| character.is_ascii_digit());
         let assigns = body
            .match_indices('=')
            .any(|(at, _)| body.as_bytes().get(at + 1) != Some(&b'>'));
         !assigns
            && ["use", "module", "submodule"]
               .iter()
               .any(|keyword| body.starts_with(keyword))
      });
   Ok((!modular).then_some(text))
}

fn run_uncached(
   invocation: &Invocation,
   format: BinaryFormat,
   compiler: &str,
   slot_socket: Option<&Path>,
) -> io::Result<RunResult> {
   let linking = invocation.links() || invocation.action == Action::Other;
   if invocation.query || !linking || format != BinaryFormat::Elf {
      return run_slotted(OsStr::new(compiler), &invocation.args, slot_socket, false);
   }
   let scratch = Builder::new()
      .prefix(".mummer-")
      .suffix(".link.d")
      .tempfile()?;
   let mut arguments = invocation.args.clone();
   let dependencies = link_depfile(&mut arguments, scratch.path());
   let (result, _linked) =
      settled_link(&invocation.output, &mut arguments, &dependencies, |args| {
         run_slotted(OsStr::new(compiler), args, slot_socket, false)
      })?;
   Ok(result)
}

pub fn uncacheable_marker(arg: &str) -> bool {
   arg.starts_with('@')
      || arg.contains(",@")
      || [
         "-Map",
         "--print-map",
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
      || arg.ends_with(",-M")
      || arg.ends_with(",-t")
}

fn given_link_depfile(args: &[String]) -> Option<&str> {
   args.iter().enumerate().find_map(|(index, arg)| {
      arg.strip_prefix("-Wl,--dependency-file=")
         .or_else(|| arg.strip_prefix("-Wl,--dependency-file,"))
         .or_else(|| {
            (index > 0 && args[index - 1] == "-Xlinker")
               .then(|| arg.strip_prefix("--dependency-file="))
               .flatten()
         })
   })
}

/// The linker dependency file `args` already name, or `ours`, added to them.
fn link_depfile(args: &mut Vec<String>, ours: &Path) -> PathBuf {
   if let Some(path) = given_link_depfile(args) {
      return PathBuf::from(path);
   }
   args.push(format!("-Wl,--dependency-file={}", ours.display()));
   ours.to_path_buf()
}

/// Links, then settles the output's RUNPATH from what the linker opened,
/// linking once more with the room the output turned out to need. Also gives
/// the linker's output as it was before settling, what the cache keeps so a
/// hit settles it again with this mummer's rules.
fn settled_link<Link>(
   output: &Path,
   args: &mut [String],
   dependencies: &Path,
   mut link: Link,
) -> io::Result<(RunResult, Option<Vec<u8>>)>
where
   Link: FnMut(&[String]) -> io::Result<RunResult>,
{
   let mut result = link(args)?;
   let mut relinked = false;
   loop {
      if result.status != 0_i32 || !output.is_file() || !dependencies.is_file() {
         return Ok((result, None));
      }
      let linked = read(output)?;
      match settle_runpath(&linked, &read_to_string(dependencies)?)? {
         Settled::Image(settled) => {
            if settled != linked {
               let mode = metadata(output)?.permissions().mode();
               write_file(output, &settled, Some(mode & 0o7777))?;
               if let Some(directory) = var_os("MUMMER_UNSETTLED_DIR")
                  && let Some(name) = output.file_name()
               {
                  write_file(&Path::new(&directory).join(name), &linked, None)?;
               }
            }
            return Ok((result, Some(linked)));
         },
         Settled::Room(_) if relinked => {
            return Err(io::Error::other(
               "RUNPATH does not fit even after relinking",
            ));
         },
         Settled::Room(room) => {
            reserve(args, room);
            result = link(args)?;
            relinked = true;
         },
      }
   }
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
      format!("l/{key}"),
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
   let linked = objects.next().flatten();
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
      let link_text = if invocation.links() {
         let Some(mut link_bytes) = linked else {
            return Err(Miss::ObjectGone);
         };
         store.mask_output(&mut link_bytes, true);
         let raw = String::from_utf8(link_bytes).map_err(io::Error::other)?;
         let text = store.resolve_all(&raw);
         let target = format!("{}:", invocation.output.display());
         Some(
            text
               .strip_prefix("<output>:")
               .map_or_else(|| text.clone(), |rest| format!("{target}{rest}")),
         )
      } else {
         None
      };
      store.mask_output(&mut bytes, true);
      store.resolve_bytes(&mut bytes);
      if let Some(ref text) = link_text {
         bytes = match settle_runpath(&bytes, text)? {
            Settled::Image(settled) => settled,
            Settled::Room(_) => return Err(Miss::RunpathRoom),
         };
      }
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
      if let (Some(given), Some(text)) = (invocation.linkfile.as_ref(), link_text.as_ref()) {
         write_file(given, text.as_bytes(), None)?;
      }
   }
   store.mask_output(&mut diagnostics, true);
   io::stderr().lock().write_all(&diagnostics)?;
   Ok(i32::from(status))
}
