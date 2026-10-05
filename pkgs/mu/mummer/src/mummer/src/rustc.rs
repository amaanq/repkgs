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
      remove_file,
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
      write_file,
   },
   process::{
      Slot,
      diagnostic,
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
use serde::{
   Deserialize,
   Serialize,
};
use tightrope::{
   client::Client,
   digest,
};

use crate::{
   driver::{
      Driver,
      Language,
   },
   manifest::{
      Manifest,
      Miss,
      lookup,
      parse_depfile,
   },
};

struct Invocation {
   key_args:  Vec<String>,
   source:    String,
   out_dir:   PathBuf,
   name:      String,
   suffix:    String,
   externs:   Vec<String>,
   search:    Vec<PathBuf>,
   statics:   Vec<String>,
   sysroot:   Option<PathBuf>,
   links:     bool,
   cacheable: bool,
   query:     bool,
}

impl Invocation {
   #[expect(
      clippy::too_many_lines,
      reason = "single-pass rustc flag scan, splitting would scatter one match"
   )]
   fn parse(args: &[String]) -> Self {
      let mut invocation = Self {
         key_args:  Vec::new(),
         source:    String::new(),
         out_dir:   PathBuf::new(),
         name:      String::new(),
         suffix:    String::new(),
         externs:   Vec::new(),
         search:    Vec::new(),
         statics:   Vec::new(),
         sysroot:   None,
         links:     false,
         cacheable: true,
         query:     false,
      };
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
                     invocation.cacheable &= !setting.starts_with("incremental=");
                  },
                  _ => {},
               }
            } else {
               invocation.cacheable = false;
            }
         } else if let Some(setting) = argument.strip_prefix("-C") {
            if let Some(suffix) = setting.strip_prefix("extra-filename=") {
               suffix.clone_into(&mut invocation.suffix);
            } else {
               invocation.key_args.push(format!("-C={setting}"));
            }
            invocation.cacheable &= !setting.starts_with("incremental=");
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
            invocation.query |= argument == "--print"
               || argument.starts_with("--print=")
               || matches!(argument.as_str(), "-" | "-vV" | "--version");
            invocation.cacheable &= !argument.starts_with("-o") && !argument.starts_with('@');
            invocation.key_args.push(argument.clone());
         }
         index += 1;
      }
      invocation.links = !library;
      invocation.cacheable &= !invocation.query
         && sources == 1_i32
         && dep_info
         && !invocation.out_dir.as_os_str().is_empty()
         && !invocation.name.is_empty();
      invocation
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

   fn static_archives(&self) -> Vec<String> {
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
         .map(|path| path.to_string_lossy().into_owned())
         .collect()
   }
}

#[derive(Deserialize, Serialize)]
struct Artifact {
   name: String,
   mode: u32,
   hash: String,
}

pub fn run_rustc(args: &[String], socket: &Path) -> io::Result<i32> {
   let Some((compiler, arguments)) = args.split_first() else {
      return Err(io::Error::other("rustcwrap requires a compiler"));
   };
   let started = Instant::now();
   let invocation = Invocation::parse(arguments);
   let label = format!("{}{}", invocation.name, invocation.suffix);
   let mut store = Store::new();
   let connection = if invocation.cacheable {
      Client::connect(socket).map(Some)
   } else {
      Ok(None)
   };
   let mut cache = match connection {
      Ok(client) => client,
      Err(error) => {
         diagnostic(format_args!("mummer Rust cache unavailable, {error}"));
         None
      },
   };
   let request = cache.as_mut().and_then(|client| {
      request_key(&store, client, compiler, &invocation)
         .inspect_err(|error| {
            diagnostic(format_args!("mummer cannot identify Rust inputs, {error}"));
         })
         .ok()
   });
   let Some((client, key)) = cache.as_mut().zip(request.as_ref()) else {
      let result = run_slotted(
         OsStr::new(compiler),
         arguments,
         (!invocation.query).then_some(socket),
         false,
      )?;
      let outcome = if invocation.query {
         "query"
      } else if invocation.cacheable {
         "no-daemon"
      } else {
         "uncacheable"
      };
      log("rustc", outcome, &label, started);
      if outcome == "uncacheable" {
         log_uncached(outcome, arguments);
      }
      return Ok(result.status);
   };
   let hit = lookup(client, &mut store, key)
      .and_then(|result| replay(client, &store, &invocation, &result));
   let miss = match hit {
      Ok(()) => {
         log("rustc", "cached", &label, started);
         return Ok(0);
      },
      Err(Miss::Failed(error)) => {
         diagnostic(format_args!(
            "mummer Rust cache replay failed, compiling again, {error}"
         ));
         Miss::Failed(error)
      },
      Err(miss) => miss,
   };
   let explained = format!("{label} {miss}");
   let (command, link_records) = invocation.recorded(arguments)?;
   let result = run(
      OsStr::new(compiler),
      &command,
      Slot::acquire(client).ok().flatten(),
      true,
      &[],
   )?;
   io::stderr().lock().write_all(&result.stderr)?;
   let storing = (result.status == 0_i32)
      .then(|| store_result(client, &mut store, key, &invocation, &result.stderr));
   for record in link_records {
      match remove_file(record) {
         Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
         Ok(()) | Err(_) => {},
      }
   }
   let Some(stored) = storing else {
      log("rustc", "failed", &explained, started);
      return Ok(result.status);
   };
   let outcome = match stored {
      Ok(()) => "compiled",
      Err(error) => {
         diagnostic(format_args!("mummer could not store Rust result, {error}"));
         "compiled-unstored"
      },
   };
   log("rustc", outcome, &explained, started);
   Ok(0)
}

fn request_key(
   store: &Store,
   cache: &mut Client,
   compiler: &str,
   invocation: &Invocation,
) -> io::Result<String> {
   let mut hasher = Hasher::new();
   field(&mut hasher, store.tool_id(compiler)?);
   field(&mut hasher, store.tool_identity(cache, compiler)?);
   field(&mut hasher, store.key(&current_dir()?.to_string_lossy()));
   for argument in &invocation.key_args {
      field(&mut hasher, store.key(argument));
   }
   field(&mut hasher, "source");
   field(&mut hasher, invocation.source.as_bytes());
   field(&mut hasher, read(&invocation.source)?);
   for external in &invocation.externs {
      field(&mut hasher, store.input_id(external)?);
   }
   if invocation.links {
      field(&mut hasher, "linker");
      field(
         &mut hasher,
         linker_identity(store, cache, invocation.linker())?,
      );
   }
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
   for library in libraries {
      field(&mut hasher, store.input_id(&library.to_string_lossy())?);
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
      field(&mut hasher, var(name).unwrap_or_default());
   }
   Ok(format!("rs/{}", &hasher.finalize().to_hex()[..32]))
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
   Ok(format!(
      "{}{}",
      store.key(&config),
      store.tool_identity(cache, driver.compiler(Language::Cc))?
   ))
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
   request: &str,
   invocation: &Invocation,
   diagnostics: &[u8],
) -> io::Result<()> {
   let dep_path = invocation
      .out_dir
      .join(format!("{}{}.d", invocation.name, invocation.suffix));
   let text = read_to_string(&dep_path)?;
   let mut inputs = parse_depfile(&text);
   inputs.extend_from_slice(&invocation.externs);
   inputs.extend(invocation.static_archives());
   inputs.push(invocation.source.clone());
   if invocation.links {
      let (depfile, linked) = invocation.link_records()?;
      let program = linker_program(&read_to_string(&linked)?);
      if program.as_deref() != Some(invocation.linker()) {
         return Err(io::Error::other(format!(
            "rustc linked with {program:?}, not the keyed {}",
            invocation.linker()
         )));
      }
      let out_dir = absolute(&invocation.out_dir)?;
      for input in parse_depfile(&read_to_string(&depfile)?) {
         let path = absolute(&input)?;
         // codegen units and rustc's tempdir, removed once linked
         if path.starts_with(&out_dir) && !path.exists() {
            continue;
         }
         inputs.push(input);
      }
   }
   inputs.sort_unstable();
   inputs.dedup();
   let variables = text
      .lines()
      .filter_map(|line| line.strip_prefix("# env-dep:"))
      .map(|entry| {
         entry
            .split_once('=')
            .map_or(entry, |(name, _)| name)
            .to_owned()
      })
      .collect::<Vec<String>>();
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
   let mut artifacts = Vec::new();
   for output in outputs {
      let name = output
         .file_name()
         .and_then(OsStr::to_str)
         .ok_or_else(|| io::Error::other("Rust output name is not UTF-8"))?;
      relative_path(name)?;
      let mut bytes = read(&output)?;
      if invocation.links {
         store.mask_output(&mut bytes, false);
      }
      let hash = digest(&bytes);
      cache.put(&format!("b/{hash}"), &bytes)?;
      artifacts.push(Artifact {
         name: stem(name, &invocation.suffix, "@"),
         mode: metadata(&output)?.permissions().mode() & 0o777,
         hash,
      });
   }
   cache.put(&format!("o/{key}"), &serde_json::to_vec(&artifacts)?)?;
   cache.put(&format!("e/{key}"), diagnostics)?;
   manifest.publish(cache, request)
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
   key: &str,
) -> Result<(), Miss> {
   let Some(listing) = cache.get(&format!("o/{key}"))? else {
      return Err(Miss::ObjectGone);
   };
   let artifacts = serde_json::from_slice::<Vec<Artifact>>(&listing).map_err(io::Error::from)?;
   if artifacts.is_empty() {
      return Err(Miss::Failed(io::Error::other(
         "cached Rust result contains no artifacts",
      )));
   }
   let mut values = Vec::new();
   for artifact in &artifacts {
      relative_path(&artifact.name)?;
      if Path::new(&artifact.name).components().count() != 1 || artifact.mode > 0o777 {
         return Err(Miss::Failed(io::Error::other(
            "invalid cached Rust artifact",
         )));
      }
      let Some(bytes) = cache.get(&format!("b/{}", artifact.hash))? else {
         return Err(Miss::ObjectGone);
      };
      if digest(&bytes) != artifact.hash {
         return Err(Miss::Failed(io::Error::other(
            "Rust artifact content hash differs",
         )));
      }
      values.push(bytes);
   }
   let Some(diagnostics) = cache.get(&format!("e/{key}"))? else {
      return Err(Miss::ObjectGone);
   };
   for (artifact, mut bytes) in artifacts.iter().zip(values) {
      if invocation.links {
         store.mask_output(&mut bytes, true);
         store.resolve_bytes(&mut bytes);
      }
      write_file(
         &invocation
            .out_dir
            .join(stem(&artifact.name, "@", &invocation.suffix)),
         &bytes,
         Some(artifact.mode),
      )?;
   }
   io::stderr().lock().write_all(&diagnostics)?;
   Ok(())
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
