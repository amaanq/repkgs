use std::{
   env::{
      current_dir,
      var,
   },
   ffi::OsStr,
   fs::{
      canonicalize,
      metadata,
      read,
      read_dir,
      read_to_string,
   },
   io::{
      self,
      Write as _,
   },
   os::unix::fs::PermissionsExt as _,
   path::{
      Path,
      PathBuf,
      absolute,
   },
   process::Command,
   time::Instant,
};

use blake3::Hasher;
use mummer::{
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
      run,
      run_slotted,
   },
   store::{
      Store,
      field,
      on_path,
   },
};
use tempfile::{
   TempDir,
   tempdir,
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
      replayed_link,
   },
   manifest::{
      Manifest,
      Miss,
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

struct Invocation {
   key_args: Vec<String>,
   source:   String,
   out_dir:  PathBuf,
   name:     String,
   suffix:   String,
   externs:  Vec<String>,
   search:   Vec<PathBuf>,
   statics:  Vec<String>,
   sysroot:  Option<PathBuf>,
   links:    bool,
}

/// A rustc call the cache cannot serve, and the crate to log it under.
struct Refused {
   label: String,
   query: bool,
}

impl Invocation {
   #[expect(
      clippy::too_many_lines,
      reason = "single-pass rustc flag scan, splitting would scatter one match"
   )]
   fn parse(args: &[String]) -> Result<Self, Refused> {
      let mut invocation = Self {
         key_args: Vec::new(),
         source:   String::new(),
         out_dir:  PathBuf::new(),
         name:     String::new(),
         suffix:   String::new(),
         externs:  Vec::new(),
         search:   Vec::new(),
         statics:  Vec::new(),
         sysroot:  None,
         links:    false,
      };
      let mut cacheable = true;
      let mut query = false;
      let mut sources = 0_i32;
      let mut dep_info = false;
      let mut library = false;
      let mut index = 0;
      while index < args.len() {
         let argument = &args[index];
         let (flag, inline) = argument
            .split_once('=')
            .map_or((argument.as_str(), None), |(flag, value)| {
               (flag, Some(value))
            });
         if [
            "--out-dir",
            "--crate-name",
            "--extern",
            "--emit",
            "--crate-type",
            "-C",
            "-L",
            "--cfg",
            "--check-cfg",
            "--cap-lints",
            "--edition",
            "--target",
            "-Z",
            "-W",
            "-A",
            "-D",
            "-F",
            "--error-format",
            "--json",
            "--diagnostic-width",
            "--remap-path-prefix",
            "--sysroot",
            "-l",
         ]
         .contains(&flag)
         {
            let value = inline.or_else(|| {
               index += 1;
               args.get(index).map(String::as_str)
            });
            if let Some(setting) = value {
               let unkeyed = matches!(flag, "--out-dir" | "-L")
                  || (flag == "-C" && setting.starts_with("extra-filename="));
               if !unkeyed {
                  invocation.key_args.push(format!("{flag}={setting}"));
               }
               match flag {
                  "--out-dir" => invocation.out_dir = PathBuf::from(setting),
                  "--sysroot" => {
                     invocation.sysroot = (!setting.is_empty()).then(|| PathBuf::from(setting));
                  },
                  "--crate-name" => setting.clone_into(&mut invocation.name),
                  "--extern" => {
                     if let Some((_, path)) = setting.split_once('=') {
                        invocation.externs.push(path.to_owned());
                     }
                  },
                  "--emit" => dep_info = setting.split(',').any(|part| part == "dep-info"),
                  "--crate-type" => {
                     library = setting
                        .split(',')
                        .all(|kind| matches!(kind, "lib" | "rlib" | "staticlib"));
                  },
                  "-L" | "-l" => invocation.link(flag, setting),
                  "-C" => {
                     if let Some(suffix) = setting.strip_prefix("extra-filename=") {
                        suffix.clone_into(&mut invocation.suffix);
                     }
                     cacheable &= !setting.starts_with("incremental=");
                  },
                  _ => {},
               }
            } else {
               cacheable = false;
            }
         } else if let Some(setting) = argument.strip_prefix("-C") {
            if let Some(suffix) = setting.strip_prefix("extra-filename=") {
               suffix.clone_into(&mut invocation.suffix);
            } else {
               invocation.key_args.push(format!("-C={setting}"));
            }
            cacheable &= !setting.starts_with("incremental=");
         } else if let Some(setting) = argument
            .strip_prefix("-L")
            .or_else(|| argument.strip_prefix("-l"))
         {
            invocation.link(&argument[..2], setting);
            invocation.key_args.push(argument.clone());
         } else if !argument.starts_with('-')
            && Path::new(argument)
               .extension()
               .is_some_and(|ext| ext.eq_ignore_ascii_case("rs"))
         {
            argument.clone_into(&mut invocation.source);
            sources += 1_i32;
         } else {
            query |= argument == "--print"
               || argument.starts_with("--print=")
               || matches!(argument.as_str(), "-" | "-vV" | "--version");
            cacheable &= !argument.starts_with("-o") && !argument.starts_with('@');
            invocation.key_args.push(argument.clone());
         }
         index += 1;
      }
      invocation.links = !library;
      let has = |prefix: &str| {
         invocation
            .key_args
            .iter()
            .any(|argument| argument.starts_with(prefix))
      };
      let foreign_linker = invocation.links && !has("-C=linker=") && has("--target=");
      if query
         || !cacheable
         || foreign_linker
         || sources != 1_i32
         || !dep_info
         || invocation.out_dir.as_os_str().is_empty()
         || invocation.name.is_empty()
      {
         return Err(Refused {
            label: invocation.label(),
            query,
         });
      }
      Ok(invocation)
   }

   fn dep_path(&self) -> PathBuf {
      self.out_dir.join(format!("{}{}.d", self.name, self.suffix))
   }

   fn label(&self) -> String {
      format!("{}{}", self.name, self.suffix)
   }

   /// Native search directories and static libraries, which an rlib bundles
   /// into itself without rustc's dep-info naming them.
   fn link(&mut self, flag: &str, setting: &str) {
      let (prefix, value) = setting.split_once('=').unwrap_or(("", setting));
      if flag == "-L" {
         if matches!(prefix, "" | "native" | "all") {
            self.search.push(PathBuf::from(value));
         }
         return;
      }
      let (kind, modifiers) = prefix.split_once(':').unwrap_or((prefix, ""));
      if kind != "static" {
         return;
      }
      let name = value.split_once(':').map_or(value, |(name, _)| name);
      if modifiers.split(',').any(|modifier| modifier == "+verbatim") {
         self.statics.push(name.to_owned());
      } else {
         self.statics.push(format!("lib{name}.a"));
         self.statics.push(format!("{name}.lib"));
      }
   }

   /// rustc's default for cc-flavored targets unless `-C linker` names one,
   /// checked against the command rustc ran before a result is stored.
   fn linker(&self) -> &str {
      self
         .key_args
         .iter()
         .rev()
         .find_map(|argument| argument.strip_prefix("-C=linker="))
         .unwrap_or("cc")
   }

   /// The linker's dependency file and rustc's record of the link command.
   fn link_records(&self) -> io::Result<(PathBuf, PathBuf)> {
      let base = absolute(&self.out_dir)?.join(format!("{}{}", self.name, self.suffix));
      Ok((
         base.with_added_extension("link.d"),
         base.with_added_extension("link-args"),
      ))
   }

   /// The command to run on a miss, which for a linking compile also writes
   /// the link records, and those records' paths.
   fn recorded(&self, arguments: &[String]) -> io::Result<(Vec<String>, Vec<PathBuf>)> {
      let mut command = arguments.to_vec();
      if !self.links {
         return Ok((command, Vec::new()));
      }
      let (depfile, linked) = self.link_records()?;
      command.push(format!(
         "-Clink-arg=-Wl,--dependency-file={}",
         depfile.display()
      ));
      command.push(format!("--print=link-args={}", linked.display()));
      Ok((command, vec![depfile, linked]))
   }

   fn static_archives(&self) -> io::Result<Vec<String>> {
      self
         .statics
         .iter()
         .filter_map(|file| {
            self
               .search
               .iter()
               .map(|directory| directory.join(file))
               .find(|path| path.is_file())
         })
         .map(|path| utf8(&path).map(str::to_owned))
         .collect()
   }
}

/// One crate compile in front of the cache.
struct Crate<'args> {
   compiler:   &'args str,
   arguments:  &'args [String],
   invocation: Invocation,
   store:      Store,
}

/// What a compile leaves behind for storing.
struct Compiled {
   link_records: Vec<PathBuf>,
   unsettled:    Option<TempDir>,
}

impl Wrapper for Crate<'_> {
   const TOOL: Tool = Tool::Rustc;
   type Compiled = Compiled;

   fn manifests(&mut self) -> &mut Store {
      &mut self.store
   }

   fn replay(&mut self, cache: &mut Client, result: &Digest) -> Result<i32, Miss> {
      replay(cache, &self.store, &self.invocation, result)
   }

   fn compile(&mut self, cache: &mut Client) -> io::Result<(RunResult, Compiled)> {
      remove_if_exists(&self.invocation.dep_path())?;
      let (command, link_records) = self.invocation.recorded(self.arguments)?;
      let unsettled = self.invocation.links.then(tempdir).transpose()?;
      let result = run(
         OsStr::new(self.compiler),
         &command,
         Slot::acquire(cache).ok().flatten(),
         unsettled
            .as_ref()
            .map(|directory| ("MUMMER_UNSETTLED_DIR", directory.path().as_os_str()))
            .as_slice(),
      )?;
      Ok((result, Compiled {
         link_records,
         unsettled,
      }))
   }

   fn store(
      &mut self,
      cache: &mut Client,
      request: &Key,
      result: &RunResult,
      compiled: Compiled,
   ) -> io::Result<bool> {
      let stored = if result.status == 0_i32 {
         store_result(
            cache,
            &mut self.store,
            request,
            &self.invocation,
            compiled.unsettled.as_ref(),
            &result.stderr,
         )
         .map(|()| true)
      } else {
         store_failure(cache, &mut self.store, request, &self.invocation, result)
      };
      for record in &compiled.link_records {
         remove_if_exists(record)?;
      }
      stored
   }
}

pub fn run_rustc(args: &[String], socket: &Path) -> io::Result<i32> {
   let Some((compiler, arguments)) = args.split_first() else {
      return Err(io::Error::other("rustcwrap requires a compiler"));
   };
   let started = Instant::now();
   let mut store = Store::new();
   let (outcome, label) = match Invocation::parse(arguments) {
      Ok(invocation) => {
         let label = invocation.label();
         match Client::connect(socket) {
            Ok(mut client) => {
               match request_key(&mut store, &mut client, compiler, &invocation) {
                  Ok(request) => {
                     let mut job = Crate {
                        compiler,
                        arguments,
                        invocation,
                        store,
                     };
                     return serve(&mut job, &mut client, &request, &label, started);
                  },
                  Err(error) => (Outcome::Unkeyed, format!("{label} {error}")),
               }
            },
            Err(error) => {
               diagnostic(format_args!("mummer Rust cache unavailable, {error}"));
               (Outcome::NoDaemon, label)
            },
         }
      },
      Err(refused) if refused.query => (Outcome::Query, refused.label),
      Err(refused) => (Outcome::Uncacheable, refused.label),
   };
   let slot = (outcome != Outcome::Query).then_some(socket);
   let result = run_slotted(OsStr::new(compiler), arguments, slot)?;
   uncached(Tool::Rustc, outcome, label, arguments, started);
   Ok(result.status)
}

fn request_key(
   store: &mut Store,
   cache: &mut Client,
   compiler: &str,
   invocation: &Invocation,
) -> io::Result<Key> {
   let mut hasher = Hasher::new();
   field(&mut hasher, store.tool_id(compiler)?);
   field(&mut hasher, store.tool_identity(cache, compiler)?);
   field(&mut hasher, store.key(utf8(&current_dir()?)?));
   for argument in &invocation.key_args {
      field(&mut hasher, store.key(argument));
   }
   field(&mut hasher, "source");
   field(&mut hasher, store.key(&invocation.source));
   field(&mut hasher, read(&invocation.source)?);
   // empty while the rust build compiles core and std themselves
   let mut libraries = match read_dir(target_libdir(compiler, invocation)?) {
      Ok(entries) => {
         entries
            .map(|entry| canonicalize(entry?.path()))
            .collect::<io::Result<Vec<_>>>()?
      },
      Err(error) if error.kind() == io::ErrorKind::NotFound => Vec::new(),
      Err(error) => return Err(error),
   };
   libraries.retain(|library| library.is_file());
   libraries.sort_unstable();
   let mut identified = invocation.externs.clone();
   for library in &libraries {
      identified.push(utf8(library)?.to_owned());
   }
   prefetch(cache, store, &identified)?;
   for external in &invocation.externs {
      field(&mut hasher, store.input_id(external)?.to_string());
   }
   if invocation.links {
      field(&mut hasher, "linker");
      field(
         &mut hasher,
         linker_identity(store, cache, invocation.linker())?,
      );
   }
   for library in libraries {
      field(&mut hasher, store.input_id(utf8(&library)?)?.to_string());
   }
   for name in [
      "CARGO_PKG_NAME",
      "CARGO_PKG_VERSION",
      "CARGO_CFG_TARGET_FEATURE",
      "RUSTFLAGS",
      "CARGO_ENCODED_RUSTFLAGS",
      "LANG",
      "LC_ALL",
   ] {
      field(&mut hasher, name);
      field(&mut hasher, store.key(&var(name).unwrap_or_default()));
   }
   Ok(Key::digest(
      Namespace::Rust,
      &Digest::from(hasher.finalize()),
   ))
}

/// A linker that is a mummer wrapper stands for the compiler its toolchain
/// configures, so mummer's own bytes stay out of the key.
fn linker_identity(store: &Store, cache: &mut Client, linker: &str) -> io::Result<String> {
   let path = on_path(linker);
   let Some(root) = path
      .parent()
      .and_then(Path::parent)
      .filter(|root| root.join("etc/mummer.json").is_file())
   else {
      return store.tool_identity(cache, linker);
   };
   let driver = Driver::load_at(root)?;
   let config = read_to_string(root.join("etc/mummer.json"))?;
   let mut identity = format!(
      "{}{}",
      store.key(&config),
      store.tool_identity(cache, driver.cc())?
   );
   for flag in driver.ldflags() {
      identity.push_str(&store.key(flag));
      if let Some(program) = flag.strip_prefix("--ld-path=") {
         identity.push_str(&store.tool_identity(cache, program)?);
      }
   }
   Ok(identity)
}

/// The standard library rustc links for this invocation, as rustc itself
/// reports it, since a toolchain's bin/ may be a launcher elsewhere.
fn target_libdir(compiler: &str, invocation: &Invocation) -> io::Result<PathBuf> {
   let mut print = Command::new(compiler);
   print.args(["--print", "target-libdir"]);
   if let Some(target) = invocation
      .key_args
      .iter()
      .find_map(|argument| argument.strip_prefix("--target="))
   {
      print.arg(format!("--target={target}"));
   }
   if let Some(ref sysroot) = invocation.sysroot {
      print.arg("--sysroot").arg(sysroot);
   }
   let output = print.output()?;
   if !output.status.success() {
      return Err(io::Error::other(format!(
         "rustc --print target-libdir failed: {}",
         String::from_utf8_lossy(&output.stderr).trim()
      )));
   }
   Ok(PathBuf::from(
      String::from_utf8_lossy(&output.stdout).trim(),
   ))
}

fn store_result(
   cache: &mut Client,
   store: &mut Store,
   request: &Key,
   invocation: &Invocation,
   unsettled: Option<&TempDir>,
   diagnostics: &[u8],
) -> io::Result<()> {
   let dep_path = invocation.dep_path();
   let text = read_to_string(&dep_path)?;
   let mut inputs = parse_depfile(&text);
   inputs.extend_from_slice(&invocation.externs);
   inputs.extend(invocation.static_archives()?);
   inputs.push(invocation.source.clone());
   let link_text = if invocation.links {
      let (depfile, linked) = invocation.link_records()?;
      let program = linker_program(&read_to_string(&linked)?);
      if program.as_deref() != Some(invocation.linker()) {
         return Err(io::Error::other(format!(
            "rustc linked with {program:?}, not the keyed {}",
            invocation.linker()
         )));
      }
      let out_dir = absolute(&invocation.out_dir)?;
      let dependencies = read_to_string(&depfile)?;
      for input in parse_depfile(&dependencies) {
         let path = absolute(&input)?;
         // codegen units and rustc's tempdir, removed once linked
         if path.starts_with(&out_dir) && !path.exists() {
            continue;
         }
         inputs.push(input);
      }
      Some(dependencies)
   } else {
      None
   };
   inputs.sort_unstable();
   inputs.dedup();
   let variables = env_deps(&text);
   let manifest = Manifest::build(cache, store, &inputs, &variables, &[])?;
   let key = manifest.result_key(request)?;
   let mut outputs = vec![dep_path];
   for line in text.lines().filter(|line| !line.starts_with('#')) {
      if let Some((target, _)) = line.split_once(": ") {
         outputs.push(PathBuf::from(target));
      }
   }
   outputs.sort_unstable();
   outputs.dedup();
   if outputs.len() <= 1 {
      return Err(io::Error::other(
         "Rust dependency file lists no compiler artifacts",
      ));
   }
   let mut record = Record {
      status:      0,
      diagnostics: diagnostics.to_vec(),
      depfile:     None,
      link_deps:   link_text
         .map(|dependencies| store.stored_text(&dependencies))
         .transpose()?,
      artifacts:   Vec::new(),
   };
   for output in outputs {
      let name = output
         .file_name()
         .and_then(OsStr::to_str)
         .ok_or_else(|| io::Error::other("Rust output name is not UTF-8"))?;
      relative_path(name)?;
      let raw = unsettled
         .map(|directory| directory.path().join(name))
         .filter(|path| path.is_file());
      let settle = raw.is_some();
      let mut bytes = read(raw.as_deref().unwrap_or(&output))?;
      if invocation.links {
         store.mask_output(&mut bytes, false);
      }
      record.artifacts.push(Artifact {
         name: stem(name, &invocation.suffix, "@"),
         mode: Some(metadata(&output)?.permissions().mode() & 0o777),
         bytes,
         settle,
      });
   }
   record.put(cache, &key)?;
   manifest.publish(cache, request)
}

/// rustc writes its dep-info before it reports an error, as for the
/// feature probes build scripts run, so a failure keys on the same inputs a
/// success would.
fn store_failure(
   cache: &mut Client,
   store: &mut Store,
   request: &Key,
   invocation: &Invocation,
   result: &RunResult,
) -> io::Result<bool> {
   let text = match read_to_string(invocation.dep_path()) {
      Ok(text) => text,
      Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
      Err(error) => return Err(error),
   };
   if invocation.links || result.status >= 128_i32 {
      return Ok(false);
   }
   let mut inputs = parse_depfile(&text);
   inputs.extend_from_slice(&invocation.externs);
   inputs.extend(invocation.static_archives()?);
   inputs.push(invocation.source.clone());
   inputs.retain(|path| Path::new(path).exists());
   inputs.sort_unstable();
   inputs.dedup();
   let variables = env_deps(&text);
   let manifest = Manifest::build(cache, store, &inputs, &variables, &[])?;
   let key = manifest.result_key(request)?;
   Record {
      status:      result.status,
      diagnostics: result.stderr.clone(),
      depfile:     None,
      link_deps:   None,
      artifacts:   Vec::new(),
   }
   .put(cache, &key)?;
   manifest.publish(cache, request)?;
   Ok(true)
}

/// The variables rustc's dep-info says the crate read.
fn env_deps(text: &str) -> Vec<String> {
   text
      .lines()
      .filter_map(|line| line.strip_prefix("# env-dep:"))
      .map(|entry| {
         entry
            .split_once('=')
            .map_or(entry, |(name, _)| name)
            .to_owned()
      })
      .collect()
}

fn stem(name: &str, from: &str, to: &str) -> String {
   if from.is_empty() {
      name.to_owned()
   } else {
      name.replace(from, to)
   }
}

fn replay(
   cache: &mut Client,
   store: &Store,
   invocation: &Invocation,
   key: &Digest,
) -> Result<i32, Miss> {
   let record = Record::get(cache, key)?;
   if record.status != 0_i32 && record.artifacts.is_empty() {
      io::stderr().lock().write_all(&record.diagnostics)?;
      return Ok(record.status);
   }
   if record.artifacts.is_empty()
      || record
         .artifacts
         .iter()
         .any(|artifact| Path::new(&artifact.name).components().count() != 1)
   {
      return Err(Miss::Failed(io::Error::other(
         "invalid cached Rust artifacts",
      )));
   }
   let link_text = if record.artifacts.iter().any(|artifact| artifact.settle) {
      let Some(stored) = record.link_deps else {
         return Err(Miss::ObjectGone);
      };
      Some(store.replayed_text(stored)?)
   } else {
      None
   };
   for artifact in record.artifacts {
      // a crate's dep-info names the vendor dir of the package that stored
      // it, another store path in this one
      let bytes = if Path::new(&artifact.name)
         .extension()
         .is_some_and(|extension| extension == "d")
         && !artifact.settle
      {
         let mut text = artifact.bytes;
         store.resolve_bytes(&mut text);
         text
      } else if invocation.links {
         let settling = link_text.as_deref().filter(|_| artifact.settle);
         replayed_link(store, artifact.bytes, settling)?
      } else {
         artifact.bytes
      };
      write_file(
         &invocation
            .out_dir
            .join(stem(&artifact.name, "@", &invocation.suffix)),
         &bytes,
         artifact.mode,
      )?;
   }
   io::stderr().lock().write_all(&record.diagnostics)?;
   Ok(0_i32)
}

/// The program in rustc's `--print link-args` record, past the environment
/// assignments it starts with.
fn linker_program(record: &str) -> Option<String> {
   let mut characters = record.chars().peekable();
   loop {
      while characters
         .next_if(|character| character.is_whitespace())
         .is_some()
      {}
      let quoted = characters.peek() == Some(&'"');
      let mut word = String::new();
      let mut inside = false;
      while let Some(character) = characters.next() {
         match character {
            '\\' if inside => word.push(characters.next()?),
            '"' => inside = !inside,
            blank if blank.is_whitespace() && !inside => break,
            other => word.push(other),
         }
      }
      if quoted {
         return Some(word);
      }
      if word.is_empty() {
         return None;
      }
   }
}
