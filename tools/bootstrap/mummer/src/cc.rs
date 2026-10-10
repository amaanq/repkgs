use std::{
   collections::BTreeSet,
   env::{
      args_os,
      current_dir,
      current_exe,
      temp_dir,
      var,
      var_os,
   },
   ffi::OsStr,
   fmt::{
      Display,
      Formatter,
      Result as FormatResult,
   },
   fs::{
      metadata,
      read,
      read_to_string,
   },
   io::{
      self,
      Write as _,
   },
   iter::once,
   mem::take,
   os::unix::fs::PermissionsExt as _,
   path::{
      Component,
      Path,
      PathBuf,
   },
   process::{
      Command,
      Stdio,
   },
   time::Instant,
};

use blake3::Hasher;
use mummer::{
   config::BinaryFormat,
   files::{
      relative_path,
      remove_if_exists,
      utf8,
      write_file,
   },
   process::{
      Outcome,
      RunResult,
      Slot,
      Tool,
      diagnostic,
      expand_response_files,
      log,
      run,
      run_slotted,
      status_code,
   },
   store::{
      Store,
      field,
      mask_hashes,
      on_path,
   },
};
use tempfile::{
   Builder,
   NamedTempFile,
};
use tightrope::{
   client::Client,
   digest::Digest,
   key::{
      Key,
      Namespace,
   },
};

use crate::{
   driver::{
      Driver,
      Language,
      Settled,
      replayed_link,
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
   record::{
      Artifact,
      Record,
   },
   wrapper::{
      Wrapper,
      serve,
      uncached,
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

/// Why a call runs without the cache.
enum Refusal {
   /// An argument with effects the key cannot see.
   Marker(String),
   Sources(i32),
   DiscardedOutput,
   /// Neither one compile nor a link of objects alone.
   Mixed,
   LinkDepfile,
   PrecompiledOutput,
   Binfmt,
   UnscannedFortran,
}

impl Display for Refusal {
   fn fmt(&self, f: &mut Formatter<'_>) -> FormatResult {
      match *self {
         Self::Marker(ref arg) => write!(f, "marker:{arg}"),
         Self::Sources(count) => write!(f, "sources:{count}"),
         Self::DiscardedOutput => f.write_str("output:/dev/null"),
         Self::Mixed => f.write_str("mixed"),
         Self::LinkDepfile => f.write_str("link-depfile"),
         Self::PrecompiledOutput => f.write_str("precompiled-output"),
         Self::Binfmt => f.write_str("binfmt"),
         Self::UnscannedFortran => f.write_str("unscanned-fortran"),
      }
   }
}

const KEYED_ENVIRONMENT: [&str; 8] = [
   "CPATH",
   "C_INCLUDE_PATH",
   "CPLUS_INCLUDE_PATH",
   "OBJC_INCLUDE_PATH",
   "LIBRARY_PATH",
   "SOURCE_DATE_EPOCH",
   "LANG",
   "LC_ALL",
];

struct Invocation {
   args:         Vec<String>,
   key_args:     Vec<String>,
   source:       Option<String>,
   target:       Option<String>,
   inputs:       Vec<String>,
   output:       PathBuf,
   depfile:      Option<PathBuf>,
   linkfile:     Option<PathBuf>,
   action:       Action,
   query:        bool,
   refused:      Option<Refusal>,
   stdout:       bool,
   preprocessed: Option<String>,
   modules:      Vec<PathBuf>,
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
         source: None,
         target: None,
         inputs: Vec::new(),
         output: PathBuf::new(),
         depfile: None,
         linkfile: None,
         action: Action::Link,
         query: false,
         refused: None,
         stdout: false,
         preprocessed: None,
         modules: Vec::new(),
         split: Vec::new(),
         jobs,
      };
      let (compile_jobs, link_jobs) = invocation.jobs.iter().partition::<Vec<_>, _>(|job| {
         job.iter()
            .skip(1)
            .find(|token| token.starts_with('-'))
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
                  invocation.target = Some(value.clone());
                  wants_deps = true;
                  consumed = true;
               }
            },
            "-MD" | "-MMD" | "-MP" => {
               keyed = false;
               wants_deps = true;
            },
            "-M" | "-MM" | "-" | "-v" | "-V" | "--version" | "-?" | "-help" | "--help" | "-###"
            | "-fsyntax-only" => {
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
                  invocation.source = Some(arg.clone());
                  sources += 1_i32;
                  positions.push(index);
                  header |= precompiles;
               } else if !arg.starts_with('-') && {
                  // a value the driver hands the linker after its option, as
                  // `-rpath <dir>` or `-z noexecstack`, is no input file
                  let option = index
                     .checked_sub(1)
                     .map(|before| invocation.args[before].as_str())
                     .filter(|before| before.starts_with('-') && !before.contains('='));
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
         if uncacheable_marker(&arg) {
            invocation
               .refused
               .get_or_insert_with(|| Refusal::Marker(arg.clone()));
         } else if passed_on
            && let Some(value) = next.as_ref().filter(|value| uncacheable_marker(value))
         {
            invocation
               .refused
               .get_or_insert_with(|| Refusal::Marker(value.clone()));
         }
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
         if sources != 1_i32 {
            invocation.refused.get_or_insert(Refusal::Sources(sources));
         }
         invocation.query |= invocation.action == Action::Preprocess && sources == 0_i32;
         if invocation.action == Action::Preprocess
            && (invocation.output.as_os_str().is_empty() || invocation.output == Path::new("-"))
         {
            invocation.stdout = true;
         } else if invocation.output.as_os_str().is_empty() && header {
            invocation.output = PathBuf::from(format!(
               "{}.gch",
               invocation.source.as_deref().unwrap_or_default()
            ));
         } else if invocation.output.as_os_str().is_empty() {
            // y.tab.c becomes y.tab.o, which set_extension would make y.o
            let stem = Path::new(invocation.source.as_deref().unwrap_or_default())
               .file_stem()
               .and_then(|stem| stem.to_str())
               .unwrap_or("output");
            let extension = if invocation.action == Action::Assembly {
               "s"
            } else {
               "o"
            };
            invocation.output = PathBuf::from(format!("{stem}.{extension}"));
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
         && invocation.refused.is_none()
         && !invocation.query
         && invocation.linkfile.is_none()
         && invocation.output != Path::new("/dev/null")
      {
         invocation.split = positions;
      }
      if invocation.output == Path::new("/dev/null") {
         invocation.refused.get_or_insert(Refusal::DiscardedOutput);
      }
      if invocation.action == Action::Other {
         invocation.refused.get_or_insert(Refusal::Mixed);
      }
      if invocation.linkfile.is_some() && !invocation.links() {
         invocation.refused.get_or_insert(Refusal::LinkDepfile);
      }
      if invocation.action != Action::Precompile
         && extension.is_some_and(|found| found == "pch" || found == "gch")
      {
         invocation.refused.get_or_insert(Refusal::PrecompiledOutput);
      }
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
   fn compile_job(&self) -> io::Result<String> {
      let mut job = self
         .jobs
         .iter()
         .map(|tokens| format!("\"{}\"", tokens.join("\" \"")))
         .collect::<Vec<_>>()
         .join("\n");
      // whole tokens, since `-o -` would otherwise rewrite every dash
      let output = utf8(&self.output)?;
      if !output.is_empty() {
         job = job.replace(&format!("\"{output}\""), "\"<output>\"");
      }
      if let Some(ref depfile) = self.depfile {
         job = job.replace(&format!("\"{}\"", utf8(depfile)?), "\"<depfile>\"");
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
      Ok(masked)
   }

   const fn links(&self) -> bool {
      matches!(self.action, Action::Link | Action::LinkSource)
   }

   /// Keeps the first reason a call cannot be cached.
   fn refuse(&mut self, refusal: Refusal) {
      self.refused.get_or_insert(refusal);
   }

   /// Keeps a Fortran compile cacheable only with its prescanned source.
   fn prescan(&mut self, language: Language, compiler: &str) -> io::Result<()> {
      if language != Language::Fortran || self.source.is_none() {
         return Ok(());
      }
      let scanned = if self.refused.is_none() && self.action == Action::Compile {
         prescanned(compiler, &self.args)?
      } else {
         None
      };
      let Some(scan) = scanned else {
         self.refuse(Refusal::UnscannedFortran);
         return Ok(());
      };
      let search = ModuleSearch::of(&self.args);
      for name in scan.used.iter().filter(|name| !scan.defined.contains(name)) {
         let found = search
            .directories()
            .map(|directory| directory.join(format!("{name}.mod")))
            .filter(|path| path.is_file())
            .map(|path| utf8(&path).map(str::to_owned))
            .collect::<io::Result<Vec<String>>>()?;
         if found.is_empty() {
            self.refuse(Refusal::UnscannedFortran);
            return Ok(());
         }
         for path in found {
            if !self.inputs.contains(&path) {
               self.inputs.push(path);
            }
         }
      }
      self.modules = scan
         .defined
         .iter()
         .map(|name| search.output.join(format!("{name}.mod")))
         .collect();
      self.preprocessed = Some(scan.text);
      Ok(())
   }

   fn key_injected(&mut self, user: &[String]) {
      for arg in &self.args {
         if !user.contains(arg) {
            self.key_args.push(arg.clone());
         }
      }
   }

   fn stored_modules(&self, store: &Store) -> io::Result<Option<Vec<Artifact>>> {
      let cwd = current_dir()?;
      let mut artifacts = Vec::with_capacity(self.modules.len());
      for module in &self.modules {
         let relative = module
            .strip_prefix(&cwd)
            .unwrap_or(module)
            .components()
            .filter(|part| *part != Component::CurDir)
            .collect::<PathBuf>();
         let name = utf8(&relative)?;
         let (Ok(_), Ok(mut bytes)) = (relative_path(name), read(module)) else {
            return Ok(None);
         };
         store.mask_output(&mut bytes, false);
         artifacts.push(Artifact {
            name: name.to_owned(),
            mode: None,
            bytes,
            settle: false,
         });
      }
      Ok(Some(artifacts))
   }

   fn retarget(&self, depfile: &str) -> String {
      let target = self
         .target
         .clone()
         .unwrap_or_else(|| self.output.display().to_string());
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

   fn request_key(&self, store: &mut Store, cache: &mut Client, compiler: &str) -> io::Result<Key> {
      let mut hasher = Hasher::new();
      field(&mut hasher, store.tool_id(compiler)?);
      field(
         &mut hasher,
         self.toolchain_identities(cache, store, compiler)?,
      );
      field(&mut hasher, store.key(utf8(&current_dir()?)?));
      field(&mut hasher, match self.action {
         Action::Link => "link",
         Action::LinkSource => "link-one",
         Action::Precompile => "precompile",
         Action::Compile | Action::Assembly | Action::Preprocess | Action::Other => "compile",
      });
      // the toolchain's own paths move with every mummer build
      let toolchain = current_exe()
         .ok()
         .and_then(|exe| Some(exe.parent()?.parent()?.to_str()?.to_owned()));
      for argument in &self.key_args {
         let mut bytes = argument.as_bytes().to_vec();
         store.mask_output(&mut bytes, false);
         let masked = String::from_utf8(bytes).map_err(io::Error::other)?;
         let embedded =
            self.links() && (argument.contains("dynamic-linker") || argument.contains("rpath"));
         field(&mut hasher, match toolchain {
            Some(ref root) if self.action == Action::Precompile => precompile_keyed(&masked, root),
            _ if embedded => store.key_embedded(&masked),
            _ => store.key(&masked),
         });
      }
      for name in KEYED_ENVIRONMENT {
         field(&mut hasher, name);
         field(&mut hasher, var(name).unwrap_or_default());
      }
      if matches!(
         self.action,
         Action::Compile | Action::Precompile | Action::Assembly | Action::Preprocess
      ) {
         let mut bytes = self.compile_job()?.into_bytes();
         store.mask_output(&mut bytes, false);
         let job = String::from_utf8(bytes).map_err(io::Error::other)?;
         field(&mut hasher, match toolchain {
            Some(ref root) if self.action == Action::Precompile => precompile_keyed(&job, root),
            _ => store.key_job(&job),
         });
      }
      if let Some(ref text) = self.preprocessed {
         field(&mut hasher, "prescanned");
         let mut bytes = store.mask_for_replay(text).into_bytes();
         store.mask_output(&mut bytes, false);
         field(&mut hasher, bytes);
      }
      if let Some(ref source) = self.source {
         field(&mut hasher, "source");
         field(&mut hasher, store.key(source));
         let mut bytes = read(source)?;
         store.mask_output(&mut bytes, false);
         field(&mut hasher, bytes);
      }
      prefetch(cache, store, &self.inputs)?;
      for path in &self.inputs {
         field(&mut hasher, store.input_id(path)?.to_string());
      }
      Ok(Key::digest(Namespace::Cc, &Digest::from(hasher.finalize())))
   }
}

/// A precompile keeps store hashes, since the headers it reads are matched
/// by exact path, but a prefix map only renames what reaches the output, and
/// bigtop maps every build tool, so its hashes stay out.
fn precompile_keyed(text: &str, root: &str) -> String {
   text
      .replace(root, "<toolchain>")
      .split('"')
      .map(|part| {
         if part.contains("prefix-map=") {
            mask_hashes(part)
         } else {
            part.to_owned()
         }
      })
      .collect::<Vec<String>>()
      .join("\"")
}

pub fn cached_header(driver: &Driver, raw: &[String], cache: &mut Client) -> io::Result<bool> {
   let expanded = driver.arguments(Language::Cc, raw);
   let mut invocation = Invocation::parse(raw.to_vec(), jobs(driver.cc(), &expanded)?);
   if invocation.action != Action::Compile || invocation.refused.is_some() {
      return Ok(false);
   }
   invocation.args = expanded;
   invocation.key_injected(raw);
   let mut store = Store::new();
   let request = invocation.request_key(&mut store, cache, driver.cc())?;
   let key = match lookup(cache, &mut store, &request) {
      Ok(key) => key,
      Err(Miss::Failed(error)) => return Err(error),
      Err(_) => return Ok(false),
   };
   match Record::get(cache, &key) {
      Ok(record) => Ok(record.status == 0_i32 && record.diagnostics.is_empty()),
      Err(Miss::Failed(error)) => Err(error),
      Err(_) => Ok(false),
   }
}

/// One compile or link in front of the cache.
struct Compile<'driver> {
   invocation: Invocation,
   compiler:   &'driver str,
   format:     BinaryFormat,
   /// Masks what is stored and replayed.
   store:      Store,
   manifests:  Store,
}

/// The files a compile wrote beside its output for storing it.
struct Scratch {
   dependency:      NamedTempFile,
   link_dependency: NamedTempFile,
   absent_log:      NamedTempFile,
   output_capture:  Option<NamedTempFile>,
   link_file:       PathBuf,
   linked:          Option<Vec<u8>>,
   traced:          Option<Vec<String>>,
   own_depfile:     bool,
   self_contained:  bool,
}

impl Scratch {
   fn dependency_path<'path>(&'path self, invocation: &'path Invocation) -> &'path Path {
      invocation
         .depfile
         .as_deref()
         .unwrap_or_else(|| self.dependency.path())
   }

   fn output_path<'path>(&'path self, invocation: &'path Invocation) -> &'path Path {
      self
         .output_capture
         .as_ref()
         .map_or(invocation.output.as_path(), NamedTempFile::path)
   }
}

impl Wrapper for Compile<'_> {
   const TOOL: Tool = Tool::Cc;
   type Compiled = Scratch;

   fn manifests(&mut self) -> &mut Store {
      &mut self.manifests
   }

   fn replay(&mut self, cache: &mut Client, result: &Digest) -> Result<i32, Miss> {
      cached(cache, &self.store, &self.invocation, result)
   }

   fn compile(&mut self, cache: &mut Client) -> io::Result<(RunResult, Scratch)> {
      let invocation = &self.invocation;
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
      let temporary = |suffix: &str| {
         Builder::new()
            .prefix(".mummer-")
            .suffix(suffix)
            .tempfile_in(parent)
      };
      let dependency = temporary(".d")?;
      let link_dependency = temporary(".link.d")?;
      let absent_log = temporary(".absent")?;
      let output_capture = invocation
         .stdout
         .then(|| Builder::new().prefix(".mummer-").suffix(".i").tempfile())
         .transpose()?;
      let source = invocation.source.as_deref().unwrap_or_default();
      let own_depfile = invocation.depfile.is_none() && invocation.action != Action::Link;
      let plain_assembly = Path::new(source)
         .extension()
         .is_some_and(|extension| extension == "s");
      let mut written = Scratch {
         link_file: link_dependency.path().to_path_buf(),
         dependency,
         link_dependency,
         absent_log,
         output_capture,
         linked: None,
         traced: None,
         own_depfile,
         self_contained: plain_assembly || invocation.preprocessed.is_some(),
      };

      let mut arguments = invocation.args.clone();
      if own_depfile && !written.self_contained {
         arguments.extend([
            "-MD".to_owned(),
            "-MF".to_owned(),
            utf8(written.dependency_path(invocation))?.to_owned(),
         ]);
      } else if !own_depfile
         && invocation.action != Action::Link
         && ![".S", ".s", ".sx"]
            .iter()
            .any(|suffix| source.ends_with(suffix))
      {
         arguments.extend(["-Xclang".to_owned(), "-sys-header-deps".to_owned()]);
      }
      if invocation.stdout {
         arguments.extend([
            "-o".to_owned(),
            utf8(written.output_path(invocation))?.to_owned(),
         ]);
      }
      if invocation.links() {
         written.link_file = link_depfile(&mut arguments, written.link_dependency.path());
      }
      let mut link = |args: &[String]| {
         run(
            OsStr::new(self.compiler),
            args,
            Slot::acquire(cache).ok().flatten(),
            &[("MUMMER_ABSENT_LOG", written.absent_log.path().as_os_str())],
         )
      };
      let result = if invocation.links() && self.format == BinaryFormat::Elf {
         let (result, linked) = settled_link(
            &invocation.output,
            &mut arguments,
            &written.link_file,
            &mut link,
         )?;
         written.linked = linked;
         result
      } else {
         link(&arguments)?
      };
      if invocation.links() && result.status != 0_i32 && only_undefined(&result.stderr) {
         written.traced = traced_inputs(self.compiler, &arguments, &result)?;
      }
      if invocation.stdout {
         io::stdout()
            .lock()
            .write_all(&read(written.output_path(invocation))?)?;
      }
      Ok((result, written))
   }

   fn store(
      &mut self,
      cache: &mut Client,
      request: &Key,
      result: &RunResult,
      compiled: Scratch,
   ) -> io::Result<bool> {
      let invocation = &self.invocation;
      let store = &self.store;
      let mut absent = read_to_string(compiled.absent_log.path())?
         .lines()
         .map(str::to_owned)
         .collect::<Vec<_>>();
      let Some((dependencies, missing_allowed)) =
         self.dependencies(result, &compiled, absent.is_empty())?
      else {
         return Ok(false);
      };
      let mut inputs = parse_depfile(&dependencies);
      if missing_allowed {
         inputs.retain(|path| Path::new(path).exists());
      }
      let link_text = if invocation.links() && result.status == 0_i32 {
         let text = read_to_string(&compiled.link_file)?;
         if text.is_empty() {
            return Ok(false);
         }
         let temporary = var("TMPDIR").unwrap_or_else(|_| "/tmp".to_owned());
         let prefix = format!("{}/", temporary.trim_end_matches('/'));
         inputs.extend(
            parse_depfile(&text)
               .into_iter()
               .filter(|path| !path.starts_with(&prefix) || path[prefix.len()..].contains('/')),
         );
         Some(text)
      } else {
         None
      };
      let traced = compiled.traced.as_ref().filter(|_| result.status != 0_i32);
      if let Some(paths) = traced {
         inputs.extend(paths.iter().cloned());
      }
      if result.status != 0_i32
         && traced.is_none()
         && (inputs.is_empty()
            || (absent.is_empty()
               && String::from_utf8_lossy(&result.stderr).contains("file not found"))
            || !result.reported_error())
      {
         return Ok(false);
      }
      inputs.extend(invocation.source.iter().cloned());
      inputs.extend_from_slice(&invocation.inputs);
      inputs.sort_unstable();
      inputs.dedup();
      if invocation.links()
         && let Some(job) = invocation.jobs.last()
      {
         absent.extend(absent_libraries(job)?);
      }
      let manifest = Manifest::build(cache, &mut self.manifests, &inputs, &[], &absent)?;
      let key = manifest.result_key(request)?;
      let mut diagnostics = result.stderr.clone();
      store.mask_output(&mut diagnostics, false);
      let mut record = Record {
         status: result.status,
         diagnostics,
         depfile: None,
         link_deps: None,
         artifacts: Vec::new(),
      };
      if result.status == 0_i32 {
         let mut object = match compiled.linked {
            Some(bytes) => bytes,
            None => read(compiled.output_path(invocation))?,
         };
         store.mask_output(&mut object, false);
         record.artifacts.push(Artifact {
            name:   match invocation.output.file_name() {
               Some(name) => utf8(Path::new(name))?.to_owned(),
               None => "output".to_owned(),
            },
            mode:   invocation.links().then_some(0o755),
            bytes:  object,
            settle: invocation.links(),
         });
         let Some(modules) = invocation.stored_modules(store)? else {
            return Ok(false);
         };
         record.artifacts.extend(modules);
         if invocation.depfile.is_some() {
            record.depfile = Some(store.stored_text(&dependencies)?);
         }
         if let Some(text) = link_text {
            let target = format!("{}:", invocation.output.display());
            let body = text
               .strip_prefix(&target)
               .map_or_else(|| text.clone(), |rest| format!("<output>:{rest}"));
            record.link_deps = Some(store.stored_text(&body)?);
         }
      }
      record.put(cache, &key)?;
      manifest.publish(cache, request)?;
      Ok(true)
   }
}

impl Compile<'_> {
   fn dependencies(
      &self,
      result: &RunResult,
      compiled: &Scratch,
      unlogged: bool,
   ) -> io::Result<Option<(String, bool)>> {
      let invocation = &self.invocation;
      if invocation.action == Action::Link {
         return Ok(Some((String::new(), false)));
      }
      if compiled.own_depfile && compiled.self_contained {
         let source = invocation.source.as_deref().unwrap_or_default();
         return Ok(Some((format!("o: {source}"), false)));
      }
      let written = match read_to_string(compiled.dependency_path(invocation)) {
         Ok(text) => Some(text).filter(|listed| !listed.is_empty()),
         Err(error) if error.kind() == io::ErrorKind::NotFound => None,
         Err(error) => return Err(error),
      };
      match written {
         Some(text) => Ok(Some((text, false))),
         None if result.status == 0_i32 => Ok(Some((String::new(), false))),
         // clang writes no dependency file for a failed compile, and only a
         // compiler that logs its absent lookups shows a missing header
         // appearing later. A located error means the linker never ran
         None if unlogged || !result.reported_error() => Ok(None),
         None => Ok(failed_dependencies(self.compiler, &invocation.args)?.map(|text| (text, true))),
      }
   }
}

pub fn run_cc(language: Language, raw: &[String], socket: &Path) -> io::Result<i32> {
   let started = Instant::now();
   let user = expand_response_files(raw)?;
   let driver = Driver::load()?;
   let Some(compiler) = driver.compiler(language) else {
      return Err(io::Error::other(
         "a Fortran compile, but etc/mummer.json names no fc",
      ));
   };

   let expanded = driver.arguments(language, &user);
   let mut invocation = Invocation::parse(user.clone(), jobs(compiler, &expanded)?);
   if !invocation.split.is_empty() && driver.format() == BinaryFormat::Elf {
      return run_split(&invocation);
   }
   if invocation.links() && driver.format() != BinaryFormat::Elf {
      invocation.refuse(Refusal::Binfmt);
   }
   invocation.args = expanded;
   invocation.prescan(language, compiler)?;
   let mut store = Store::new();
   let mut manifests = invocation.manifest_store();
   store.learn_random(&invocation.args);
   manifests.learn_random(&invocation.args);
   invocation.key_injected(&user);
   let subject = invocation
      .source
      .clone()
      .unwrap_or_else(|| invocation.output.display().to_string());

   if let Some(status) = served_uncompiled(&invocation, &user, compiler, socket, started)? {
      return Ok(status);
   }
   let outcome = if invocation.query {
      Outcome::Query
   } else if let Some(ref refusal) = invocation.refused {
      let result = run_uncached(&invocation, driver.format(), compiler, Some(socket))?;
      let outcome = if invocation.links() || invocation.action == Action::Other {
         Outcome::Linked
      } else {
         Outcome::Uncacheable
      };
      uncached(
         Tool::Cc,
         outcome,
         format_args!("{subject} {refusal}"),
         &user,
         started,
      );
      return Ok(result.status);
   } else {
      match Client::connect(socket) {
         Ok(mut cache) => {
            match invocation.request_key(&mut store, &mut cache, compiler) {
               Ok(request) => {
                  let mut job = Compile {
                     invocation,
                     compiler,
                     format: driver.format(),
                     store,
                     manifests,
                  };
                  return serve(&mut job, &mut cache, &request, &subject, started);
               },
               Err(error) => {
                  let result = run_uncached(&invocation, driver.format(), compiler, Some(socket))?;
                  uncached(
                     Tool::Cc,
                     Outcome::Unkeyed,
                     format_args!("{subject} {error}"),
                     &user,
                     started,
                  );
                  return Ok(result.status);
               },
            }
         },
         Err(error)
            if matches!(
               error.kind(),
               io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
            ) =>
         {
            Outcome::NoDaemon
         },
         Err(error) => {
            diagnostic(format_args!(
               "mummer compilation cache unavailable, {error}"
            ));
            Outcome::NoDaemon
         },
      }
   };
   let slot = (outcome != Outcome::Query).then_some(socket);
   let result = run_uncached(&invocation, driver.format(), compiler, slot)?;
   uncached(Tool::Cc, outcome, &subject, &user, started);
   Ok(result.status)
}

fn served_uncompiled(
   invocation: &Invocation,
   user: &[String],
   compiler: &str,
   socket: &Path,
   started: Instant,
) -> io::Result<Option<i32>> {
   if let Some(status) = served_query(invocation, user, compiler, socket, started)? {
      return Ok(Some(status));
   }
   let rejected = invocation.jobs.is_empty()
      && !invocation.query
      && matches!(
         invocation.action,
         Action::Compile | Action::Assembly | Action::Preprocess
      );
   if !rejected {
      return Ok(None);
   }
   served_rejection(invocation, user, compiler, socket, started)
}

/// A query only the compiler answers, `-?`, `--version` or `as` on /dev/null.
fn served_query(
   invocation: &Invocation,
   user: &[String],
   compiler: &str,
   socket: &Path,
   started: Instant,
) -> io::Result<Option<i32>> {
   let null = |path: &str| path == "/dev/null";
   let discarded = invocation.output == Path::new("/dev/null")
      && invocation.source.as_deref().is_none_or(null)
      && invocation.inputs.iter().all(|input| null(input));
   let reads_files = user.iter().any(|arg| {
      matches!(arg.as_str(), "-" | "-include" | "-imacros")
         || discarded && matches!(arg.as_str(), "-v" | "-###")
         || [
            "-print-file-name",
            "--print-file-name",
            "-print-prog-name",
            "--print-prog-name",
         ]
         .iter()
         .any(|prefix| arg.starts_with(prefix))
   });
   // nothing to link reaches only the linker's own options, as in meson's
   // `-Wl,--version`
   let inputless = invocation.action == Action::Other
      && invocation.source.is_none()
      && invocation.inputs.is_empty()
      && user.iter().all(|arg| {
         arg.strip_prefix("-Wl,").map_or_else(
            || !arg.starts_with("-l") && arg != "-Xlinker",
            |words| {
               words
                  .split(',')
                  .all(|word| word.starts_with('-') && !word.contains('='))
            },
         )
      });
   if !(invocation.query || discarded || inputless)
      || reads_files
      || invocation
         .source
         .as_deref()
         .is_some_and(|source| !null(source))
      || !invocation.inputs.iter().all(|input| null(input))
   {
      return Ok(None);
   }
   let Ok(mut cache) = Client::connect(socket) else {
      return Ok(None);
   };
   let store = Store::new();
   let mut hasher = Hasher::new();
   field(&mut hasher, "query");
   field(&mut hasher, store.tool_identity(&mut cache, compiler)?);
   for argument in &invocation.args {
      field(&mut hasher, store.key(argument));
   }
   for name in KEYED_ENVIRONMENT {
      field(&mut hasher, name);
      field(&mut hasher, var(name).unwrap_or_default());
   }
   let key = Digest::from(hasher.finalize());
   // the key is the compiler's content, so a hit can come from a toolchain
   // at another store path, which `-print-search-dirs` and the like print
   if let Ok(mut record) = Record::get(&mut cache, &key) {
      for artifact in &mut record.artifacts {
         store.resolve_bytes(&mut artifact.bytes);
         io::stdout().lock().write_all(&artifact.bytes)?;
      }
      store.resolve_bytes(&mut record.diagnostics);
      io::stderr().lock().write_all(&record.diagnostics)?;
      log(Tool::Cc, Outcome::Cached, "query", started);
      return Ok(Some(record.status));
   }
   let output = Command::new(compiler)
      .args(&invocation.args)
      .stdin(Stdio::null())
      .output()?;
   io::stdout().lock().write_all(&output.stdout)?;
   io::stderr().lock().write_all(&output.stderr)?;
   let status = status_code(output.status);
   Record {
      status,
      diagnostics: output.stderr,
      depfile: None,
      link_deps: None,
      artifacts: vec![Artifact {
         name:   "stdout".to_owned(),
         mode:   None,
         bytes:  output.stdout,
         settle: false,
      }],
   }
   .put(&mut cache, &key)?;
   log(Tool::Cc, Outcome::Compiled, "query", started);
   Ok(Some(status))
}

/// A compile the driver refuses, which fails alike until its inputs change.
fn served_rejection(
   invocation: &Invocation,
   user: &[String],
   compiler: &str,
   socket: &Path,
   started: Instant,
) -> io::Result<Option<i32>> {
   let Ok(mut cache) = Client::connect(socket) else {
      return Ok(None);
   };
   let store = Store::new();
   let mut hasher = Hasher::new();
   field(&mut hasher, "rejected");
   field(&mut hasher, store.tool_identity(&mut cache, compiler)?);
   field(&mut hasher, store.key(utf8(&current_dir()?)?));
   for argument in &invocation.args {
      field(&mut hasher, store.key(argument));
   }
   for name in KEYED_ENVIRONMENT {
      field(&mut hasher, name);
      field(&mut hasher, var(name).unwrap_or_default());
   }
   for argument in user.iter().filter(|argument| !argument.starts_with('-')) {
      match read(argument) {
         Ok(mut bytes) => {
            store.mask_output(&mut bytes, false);
            field(&mut hasher, bytes);
         },
         Err(_) => field(&mut hasher, "absent"),
      }
   }
   let key = Digest::from(hasher.finalize());
   if let Ok(record) = Record::get(&mut cache, &key) {
      io::stderr().lock().write_all(&record.diagnostics)?;
      log(Tool::Cc, Outcome::CachedError, "rejected", started);
      return Ok(Some(record.status));
   }
   let output = Command::new(compiler)
      .args(&invocation.args)
      .stdin(Stdio::null())
      .output()?;
   io::stdout().lock().write_all(&output.stdout)?;
   io::stderr().lock().write_all(&output.stderr)?;
   let status = status_code(output.status);
   if status == 0_i32 || !output.stdout.is_empty() {
      log(Tool::Cc, Outcome::Uncacheable, "rejected-but-ran", started);
      return Ok(Some(status));
   }
   Record {
      status,
      diagnostics: output.stderr,
      depfile: None,
      link_deps: None,
      artifacts: Vec::new(),
   }
   .put(&mut cache, &key)?;
   log(Tool::Cc, Outcome::CompiledError, "rejected", started);
   Ok(Some(status))
}

/// The jobs the compiler driver would run for `args`, each as its tokens,
/// or none when it refuses them, which leaves the call uncacheable.
pub fn jobs(compiler: &str, args: &[String]) -> io::Result<Vec<Vec<String>>> {
   let printed = Command::new(compiler).arg("-###").args(args).output()?;
   if !printed.status.success() {
      return Ok(Vec::new());
   }
   let Ok(text) = String::from_utf8(printed.stderr) else {
      return Ok(Vec::new());
   };
   Ok(text
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
      .ok_or_else(|| io::Error::other("mummer has no program name"))
      .and_then(|name| Ok(on_path(utf8(Path::new(&name))?)))?;
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

fn failed_dependencies(compiler: &str, args: &[String]) -> io::Result<Option<String>> {
   let listing = Builder::new().prefix(".mummer-").suffix(".d").tempfile()?;
   let mut arguments = Vec::with_capacity(args.len() + 4);
   let mut words = args.iter();
   while let Some(arg) = words.next() {
      match arg.as_str() {
         "-c" | "-MD" | "-MMD" | "-MP" => {},
         "-o" | "-MF" | "-MT" | "-MQ" => {
            words.next();
         },
         _ if arg.starts_with("-Wp,-M") || arg == "-pedantic-errors" => {},
         _ => arguments.push(arg.as_str()),
      }
   }
   // the listing only needs the headers, and a -Werror build's warnings
   // would fail the pass that finds them
   arguments.extend(["-w", "-M", "-MG", "-MF", utf8(listing.path())?]);
   let output = Command::new(compiler).args(&arguments).output()?;
   if !output.status.success() {
      return Ok(None);
   }
   Ok(Some(read_to_string(listing.path())?).filter(|text| !text.is_empty()))
}

/// The source as flang prescans it with every INCLUDE inlined and the
/// modules it uses and defines, or None when that fails or it has a
/// submodule, whose `.smod` files the key does not follow.
fn prescanned(compiler: &str, args: &[String]) -> io::Result<Option<Scan>> {
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
   let Ok(text) = String::from_utf8(output.stdout) else {
      return Ok(None);
   };
   let mut used = Vec::new();
   let mut defined = Vec::new();
   for statement in text
      .lines()
      .filter(|line| !line.starts_with('#'))
      .flat_map(|line| line.split(';'))
   {
      // fixed form ignores blanks, so "U S E M" is a USE statement and only an
      // = tells USEEI = .TRUE. apart from one
      let compact = statement
         .chars()
         .filter(|character| !character.is_whitespace())
         .collect::<String>()
         .to_ascii_lowercase();
      let body = compact.trim_start_matches(|character: char| character.is_ascii_digit());
      let assigns = body
         .match_indices('=')
         .any(|(at, _)| body.as_bytes().get(at + 1) != Some(&b'>'));
      if assigns {
         continue;
      }
      if body.starts_with("submodule") {
         return Ok(None);
      }
      if let Some(rest) = body.strip_prefix("use") {
         let named = [",intrinsic::", ",non_intrinsic::", "::"]
            .iter()
            .find_map(|prefix| rest.strip_prefix(prefix))
            .unwrap_or(rest);
         let name = named
            .split(|character: char| !(character.is_ascii_alphanumeric() || character == '_'))
            .next()
            .unwrap_or_default();
         if !name.is_empty() && !used.iter().any(|known| known == name) {
            used.push(name.to_owned());
         }
         continue;
      }
      let tokens = statement.split_whitespace().collect::<Vec<&str>>();
      if let [keyword, name] = *tokens.as_slice()
         && keyword.eq_ignore_ascii_case("module")
      {
         defined.push(name.to_ascii_lowercase());
      }
   }
   Ok(Some(Scan {
      text,
      used,
      defined,
   }))
}

struct Scan {
   text:    String,
   used:    Vec<String>,
   defined: Vec<String>,
}

struct ModuleSearch {
   output:    PathBuf,
   includes:  Vec<PathBuf>,
   intrinsic: Vec<PathBuf>,
}

impl ModuleSearch {
   fn of(args: &[String]) -> Self {
      let mut search = Self {
         output:    PathBuf::from("."),
         includes:  Vec::new(),
         intrinsic: Vec::new(),
      };
      let mut words = args.iter();
      while let Some(arg) = words.next() {
         match arg.as_str() {
            "-J" | "-module-dir" => {
               search.output = words.next().map(PathBuf::from).unwrap_or_default();
            },
            "-I" => search.includes.extend(words.next().map(PathBuf::from)),
            "-fintrinsic-modules-path" => search.intrinsic.extend(words.next().map(PathBuf::from)),
            _ => {
               if let Some(directory) = arg.strip_prefix("-J") {
                  search.output = PathBuf::from(directory);
               } else if let Some(directory) = arg.strip_prefix("-I") {
                  search.includes.push(PathBuf::from(directory));
               }
            },
         }
      }
      search
   }

   fn directories(&self) -> impl Iterator<Item = &PathBuf> {
      once(&self.output)
         .chain(&self.includes)
         .chain(&self.intrinsic)
   }
}

fn run_uncached(
   invocation: &Invocation,
   format: BinaryFormat,
   compiler: &str,
   slot_socket: Option<&Path>,
) -> io::Result<RunResult> {
   let linking = invocation.links() || invocation.action == Action::Other;
   if invocation.query || !linking || format != BinaryFormat::Elf {
      return run_slotted(OsStr::new(compiler), &invocation.args, slot_socket);
   }
   let scratch = Builder::new()
      .prefix(".mummer-")
      .suffix(".link.d")
      .tempfile()?;
   let mut arguments = invocation.args.clone();
   let dependencies = link_depfile(&mut arguments, scratch.path());
   let (result, _linked) =
      settled_link(&invocation.output, &mut arguments, &dependencies, |args| {
         run_slotted(OsStr::new(compiler), args, slot_socket)
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
         "-fstack-usage",
         "-fsave-optimization-record",
         "-ftest-coverage",
         "-save-stats",
         "-fplugin",
         "-fpass-plugin",
         "--why-extract",
         "--print-archive-stats",
         "--reproduce",
         "--time-trace",
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

/// The library files a link would have taken over the ones it found, had
/// they existed: each `-l` name's candidates in the search directories
/// before the one that satisfied it, in lld's order of `.so` then `.a`.
fn absent_libraries(job: &[String]) -> io::Result<Vec<String>> {
   let cwd = current_dir()?;
   let mut directories = Vec::new();
   let mut wanted = Vec::new();
   let mut only_static = false;
   let mut tokens = job.iter().skip(1);
   while let Some(token) = tokens.next() {
      match token.as_str() {
         "-L" | "--library-path" => directories.extend(tokens.next().map(PathBuf::from)),
         "-l" | "--library" => wanted.extend(tokens.next().map(|name| (name.clone(), only_static))),
         "-Bstatic" | "-static" | "-dn" | "-non_shared" => only_static = true,
         "-Bdynamic" | "-dy" | "-call_shared" => only_static = false,
         other => {
            if let Some(directory) = other
               .strip_prefix("--library-path=")
               .or_else(|| other.strip_prefix("-L"))
            {
               directories.push(PathBuf::from(directory));
            } else if let Some(name) = other
               .strip_prefix("--library=")
               .or_else(|| other.strip_prefix("-l"))
            {
               wanted.push((name.to_owned(), only_static));
            }
         },
      }
   }
   let mut absent = Vec::new();
   for (name, archive_only) in wanted {
      let files = match name.strip_prefix(':') {
         Some(exact) => vec![exact.to_owned()],
         None if archive_only => vec![format!("lib{name}.a")],
         None => vec![format!("lib{name}.so"), format!("lib{name}.a")],
      };
      'search: for directory in &directories {
         for file in &files {
            let path = cwd.join(directory).join(file);
            if path.exists() {
               break 'search;
            }
            absent.push(utf8(&path)?.to_owned());
         }
      }
   }
   Ok(absent)
}

/// A link that failed only on undefined symbols found every library it
/// asked for, so what it read decides the failure as fully as a success.
fn only_undefined(stderr: &[u8]) -> bool {
   let text = String::from_utf8_lossy(stderr);
   let errors = text
      .lines()
      .filter(|line| line.contains("error:"))
      .collect::<Vec<&str>>();
   errors
      .iter()
      .any(|line| line.to_lowercase().contains("undefined symbol"))
      && errors.iter().all(|line| {
         let lowered = line.to_lowercase();
         lowered.contains("undefined symbol") || lowered.contains("linker command failed")
      })
}

/// lld writes no dependency file when a link fails, so the link runs once
/// more with `--trace` to name the files it read, the compiler's own
/// scratch objects left out as the dependency file leaves them.
fn traced_inputs(
   compiler: &str,
   arguments: &[String],
   failed: &RunResult,
) -> io::Result<Option<Vec<String>>> {
   let output = Command::new(compiler)
      .args(arguments)
      .arg("-Wl,--trace")
      .stdin(Stdio::null())
      .output()?;
   if status_code(output.status) != failed.status || !only_undefined(&output.stderr) {
      return Ok(None);
   }
   let temporary = var("TMPDIR").unwrap_or_else(|_| "/tmp".to_owned());
   let prefix = format!("{}/", temporary.trim_end_matches('/'));
   let paths = String::from_utf8_lossy(&output.stdout)
      .lines()
      .map(|line| {
         line
            .split_once('(')
            .map_or(line, |pair| pair.0)
            .trim()
            .to_owned()
      })
      .filter(|path| !path.is_empty() && Path::new(path).is_file())
      .filter(|path| !path.starts_with(&prefix) || path[prefix.len()..].contains('/'))
      .collect::<BTreeSet<String>>();
   Ok((!paths.is_empty()).then(|| paths.into_iter().collect()))
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
   key: &Digest,
) -> Result<i32, Miss> {
   let Record {
      status,
      mut diagnostics,
      depfile,
      link_deps,
      artifacts,
   } = Record::get(cache, key)?;
   if status == 0_i32 {
      let mut written = artifacts.into_iter();
      let Some(object) = written.next() else {
         return Err(Miss::ObjectGone);
      };
      let dep_text = if invocation.depfile.is_some() {
         let Some(text) = depfile else {
            return Err(Miss::ObjectGone);
         };
         Some(invocation.retarget(&store.replayed_text(text)?))
      } else {
         None
      };
      let link_text = if invocation.links() {
         let Some(stored) = link_deps else {
            return Err(Miss::ObjectGone);
         };
         let text = store.replayed_text(stored)?;
         let target = format!("{}:", invocation.output.display());
         Some(
            text
               .strip_prefix("<output>:")
               .map_or_else(|| text.clone(), |rest| format!("{target}{rest}")),
         )
      } else {
         None
      };
      let settling = link_text.as_deref().filter(|_| object.settle);
      let bytes = replayed_link(store, object.bytes, settling)?;
      if invocation.stdout {
         io::stdout().lock().write_all(&bytes)?;
      } else {
         write_file(&invocation.output, &bytes, object.mode)?;
      }
      for module in written {
         let replayed = replayed_link(store, module.bytes, None)?;
         write_file(Path::new(&module.name), &replayed, module.mode)?;
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
   Ok(status)
}
