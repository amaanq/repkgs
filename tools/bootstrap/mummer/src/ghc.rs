//! One `ghc -c` per module, as hadrian runs it, served from the cache. The
//! inputs are the `.hi` and header files hadrian's `.dependencies` lists for
//! the object, which `ghc -M` wrote with the same flags.

use std::{
   collections::{
      BTreeSet,
      HashMap,
   },
   env::{
      current_dir,
      var,
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
   path::Path,
   process::Command,
   time::{
      Instant,
      SystemTime,
   },
};

use blake3::Hasher;
use mummer::{
   files::{
      relative_path,
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
   },
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
   manifest::{
      Manifest,
      Miss,
      parse_depfile,
      remember_written,
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

const SOURCES: [&str; 5] = [".hs", ".lhs", ".hs-boot", ".lhs-boot", ".cmm"];

struct Invocation {
   source:   String,
   /// Its headers are in hadrian's `<object>.d`.
   cmm:      bool,
   output:   String,
   outputs:  Vec<String>,
   required: usize,
   files:    Vec<String>,
   suffixes: (String, String),
}

impl Invocation {
   /// The compile of one module, else the label to log the call under.
   #[expect(
      clippy::too_many_lines,
      reason = "single-pass ghc flag scan, splitting would scatter invocation state"
   )]
   fn parse(args: &[String]) -> Result<Self, String> {
      let mut sources = Vec::new();
      let mut named_output = None;
      let mut interface = None;
      let mut object_suffix = "o".to_owned();
      let mut interface_suffix = "hi".to_owned();
      let mut dynamic_object = "dyn_o".to_owned();
      let mut dynamic_interface = "dyn_hi".to_owned();
      let mut compile = false;
      let mut dynamic_too = false;
      let mut cacheable = true;
      let mut complete = true;
      let mut directories = Vec::new();
      let mut files = Vec::new();
      let mut index = 0;

      while index < args.len() {
         let argument = args[index].as_str();
         let mut value = || {
            index += 1;
            let found = args.get(index).cloned();
            complete &= found.is_some();
            found
         };

         match argument {
            "-c" => compile = true,
            "-dynamic-too" => dynamic_too = true,
            "-o" => named_output = value(),
            "-ohi" => interface = value(),
            "-osuf" => object_suffix = value().unwrap_or_default(),
            "-hisuf" => interface_suffix = value().unwrap_or_default(),
            "-dynosuf" => dynamic_object = value().unwrap_or_default(),
            "-dynhisuf" => dynamic_interface = value().unwrap_or_default(),
            "-outputdir" | "-odir" | "-hidir" | "-stubdir" | "-dumpdir" => {
               directories.extend(value());
            },
            "-dyno" | "-dynohi" | "-hiedir" => cacheable = false,
            _ if argument == "-fwrite-ide-info" || argument.starts_with("-ddump") => {
               cacheable = false;
            },
            _ if !argument.starts_with('-')
               && SOURCES.iter().any(|suffix| argument.ends_with(suffix)) =>
            {
               sources.push(argument.to_owned());
            },
            _ => {
               let named = ["-optP", "-optc", "-ghcversion-file="]
                  .iter()
                  .find_map(|prefix| argument.strip_prefix(prefix))
                  .unwrap_or(argument);
               if Path::new(named).is_file() {
                  files.push(named.to_owned());
               }
            },
         }
         index += 1;
      }

      if sources.len() != 1 {
         return Err(String::new());
      }
      let source = sources.swap_remove(0);
      let boot = if source.ends_with("-boot") {
         "-boot"
      } else {
         ""
      };
      let Some((output, stem)) = named_output.and_then(|output| {
         let stem = output
            .strip_suffix(&format!(".{object_suffix}{boot}"))?
            .to_owned();
         Some((output, stem))
      }) else {
         return Err(source);
      };
      if !(cacheable
         && complete
         && compile
         && relative_path(&output).is_ok()
         && directories
            .iter()
            .all(|directory| Path::new(&output).starts_with(directory))
         && interface
            .as_deref()
            .is_none_or(|path| relative_path(path).is_ok()))
      {
         return Err(source);
      }

      let cmm = Path::new(&source)
         .extension()
         .is_some_and(|extension| extension.eq_ignore_ascii_case("cmm"));
      let (outputs, required) = if cmm {
         let mut outputs = vec![output.clone()];
         // ghc ignores -dynamic-too for Cmm, hadrian compiles the dyn way apart
         if dynamic_too {
            outputs.push(format!("{stem}.{dynamic_object}"));
         }
         (outputs, 1)
      } else {
         let mut outputs = vec![
            output.clone(),
            interface.unwrap_or_else(|| format!("{stem}.{interface_suffix}{boot}")),
         ];
         // ghc writes no dynamic object for a boot interface
         let required = if dynamic_too && boot.is_empty() { 4 } else { 2 };
         if dynamic_too {
            outputs.extend([
               format!("{stem}.{dynamic_object}{boot}"),
               format!("{stem}.{dynamic_interface}{boot}"),
            ]);
         }
         outputs.push(format!("{stem}_stub.h"));
         (outputs, required)
      };
      files.retain(|file| *file != source && !outputs.contains(file));

      Ok(Self {
         source,
         cmm,
         output,
         outputs,
         required,
         files,
         suffixes: (object_suffix, interface_suffix),
      })
   }

   /// Everything the object depends on in the nearest `.dependencies` above
   /// it, following interfaces in the same package transitively since hadrian
   /// lists direct imports only.
   fn dependencies(&self) -> io::Result<Vec<String>> {
      if self.cmm {
         let listed = read_to_string(format!("{}.d", self.output))?;
         return Ok(parse_depfile(&listed)
            .into_iter()
            .filter(|path| *path != self.source && Path::new(path).is_file())
            .collect());
      }
      let (ref object_suffix, ref interface_suffix) = self.suffixes;
      let interface_of = |object: &str| {
         ["", "-boot"].into_iter().find_map(|boot| {
            object
               .strip_suffix(&format!(".{object_suffix}{boot}"))
               .map(|stem| format!("{stem}.{interface_suffix}{boot}"))
         })
      };
      let mut directory = Path::new(&self.output).parent();

      while let Some(current) = directory.filter(|path| !path.as_os_str().is_empty()) {
         let candidate = current.join(".dependencies");
         if !candidate.is_file() {
            directory = current.parent();
            continue;
         }

         let text = read_to_string(&candidate)?;
         let lines = text
            .lines()
            .filter_map(|line| {
               let mut tokens = line.split_whitespace();
               Some((tokens.next()?, tokens.collect::<Vec<_>>()))
            })
            .collect::<HashMap<_, _>>();
         let objects = lines
            .keys()
            .filter_map(|&object| Some((interface_of(object)?, object)))
            .collect::<HashMap<_, _>>();
         if !lines.contains_key(self.output.as_str()) {
            return Err(io::Error::other(format!(
               "{} lists no {}",
               candidate.display(),
               self.output
            )));
         }

         let mut found = BTreeSet::new();
         let mut pending = vec![self.output.as_str()];
         while let Some(object) = pending.pop() {
            for &dependency in lines.get(object).into_iter().flatten() {
               if found.insert(dependency.to_owned())
                  && let Some(&next) = objects.get(dependency)
               {
                  pending.push(next);
               }
            }
         }
         return Ok(found.into_iter().collect());
      }
      Err(io::Error::other(format!(
         "no .dependencies above {}",
         self.output
      )))
   }
}

/// One module compile in front of the cache, and what serving it needs.
struct Module<'args> {
   compiler:     &'args str,
   arguments:    &'args [String],
   invocation:   Invocation,
   store:        Store,
   dependencies: Vec<String>,
}

impl Wrapper for Module<'_> {
   const TOOL: Tool = Tool::Ghc;
   type Compiled = Vec<Option<SystemTime>>;

   fn manifests(&mut self) -> &mut Store {
      &mut self.store
   }

   fn replay(&mut self, cache: &mut Client, result: &Digest) -> Result<i32, Miss> {
      replay(cache, &self.store, result).map(|()| 0_i32)
   }

   fn compile(&mut self, cache: &mut Client) -> io::Result<(RunResult, Self::Compiled)> {
      let before = modified(&self.invocation.outputs);
      let result = run(
         OsStr::new(self.compiler),
         self.arguments,
         Slot::acquire(cache).ok().flatten(),
         &[],
      )?;
      Ok((result, before))
   }

   fn store(
      &mut self,
      cache: &mut Client,
      request: &Key,
      result: &RunResult,
      compiled: Self::Compiled,
   ) -> io::Result<bool> {
      if result.status != 0_i32 {
         return Ok(false);
      }
      let before = compiled;
      let invocation = &self.invocation;
      let mut inputs = take(&mut self.dependencies);
      inputs.push(invocation.source.clone());
      inputs.extend_from_slice(&invocation.files);
      inputs.sort_unstable();
      inputs.dedup();
      let after = modified(&invocation.outputs);
      let manifest = Manifest::build(cache, &mut self.store, &inputs, &[], &[])?;
      let key = manifest.result_key(request)?;
      let mut record = Record {
         status:      0,
         diagnostics: result.stderr.clone(),
         depfile:     None,
         link_deps:   None,
         artifacts:   Vec::new(),
      };

      for (position, path) in invocation.outputs.iter().enumerate() {
         if after[position].is_none() || after[position] == before[position] {
            if position < invocation.required {
               return Err(io::Error::other(format!(
                  "{path} was not written by this compile"
               )));
            }
            continue;
         }
         let mut bytes =
            read(path).map_err(|error| io::Error::other(format!("{path}: {error}")))?;
         self.store.mask_output(&mut bytes, false);
         record.artifacts.push(Artifact {
            name: path.clone(),
            mode: Some(metadata(path)?.permissions().mode() & 0o777),
            bytes,
            settle: false,
         });
      }

      record.put(cache, &key)?;
      manifest.publish(cache, request)?;
      Ok(true)
   }
}

pub fn run_ghc(args: &[String], socket: &Path) -> io::Result<i32> {
   let Some((compiler, arguments)) = args.split_first() else {
      return Err(io::Error::other("ghcwrap requires a compiler"));
   };
   let started = Instant::now();
   let store = Store::new();
   if arguments.iter().any(|argument| argument == "-M")
      && let Some(status) = served_scan(compiler, arguments, socket, started)?
   {
      return Ok(status);
   }
   let (outcome, label) = match Invocation::parse(arguments) {
      Ok(invocation) => {
         match prepare(&store, compiler, arguments, &invocation, socket) {
            Some((mut client, request, dependencies)) => {
               let label = invocation.source.clone();
               let mut module = Module {
                  compiler,
                  arguments,
                  invocation,
                  store,
                  dependencies,
               };
               return serve(&mut module, &mut client, &request, &label, started);
            },
            None => (Outcome::NoDaemon, invocation.source),
         }
      },
      Err(label) => (Outcome::Uncacheable, label),
   };
   let result = run_slotted(OsStr::new(compiler), arguments, Some(socket))?;
   uncached(Tool::Ghc, outcome, label, arguments, started);
   Ok(result.status)
}

/// A `-M` run, as hadrian's per-way dependency scan of a package, whose one
/// output is the makefile and whose inputs are the files that makefile
/// lists.
struct Scan<'call> {
   compiler:  &'call str,
   arguments: &'call [String],
   makefile:  String,
   store:     Store,
}

impl Wrapper for Scan<'_> {
   const TOOL: Tool = Tool::Ghc;
   type Compiled = ();

   fn manifests(&mut self) -> &mut Store {
      &mut self.store
   }

   fn replay(&mut self, cache: &mut Client, result: &Digest) -> Result<i32, Miss> {
      let record = Record::get(cache, result)?;
      let Some(artifact) = record.artifacts.first() else {
         return Err(Miss::Failed(io::Error::other(
            "cached scan has no makefile",
         )));
      };
      let text = String::from_utf8(artifact.bytes.clone()).map_err(io::Error::other)?;
      write_file(
         Path::new(&self.makefile),
         self.store.replayed_text(text)?.as_bytes(),
         None,
      )?;
      io::stderr().lock().write_all(&record.diagnostics)?;
      Ok(0_i32)
   }

   fn compile(&mut self, cache: &mut Client) -> io::Result<(RunResult, ())> {
      let result = run(
         OsStr::new(self.compiler),
         self.arguments,
         Slot::acquire(cache).ok().flatten(),
         &[],
      )?;
      Ok((result, ()))
   }

   fn store(
      &mut self,
      cache: &mut Client,
      request: &Key,
      result: &RunResult,
      (): (),
   ) -> io::Result<bool> {
      if result.status != 0_i32 {
         return Ok(false);
      }
      let text = read_to_string(&self.makefile)?;
      let mut inputs = text
         .replace("\\\n", " ")
         .lines()
         .filter(|line| !line.starts_with('#'))
         .filter_map(|line| line.split_once(':').map(|rule| rule.1))
         .flat_map(str::split_whitespace)
         .filter(|path| Path::new(path).is_file())
         .map(str::to_owned)
         .collect::<Vec<String>>();
      inputs.extend(scanned_sources(self.arguments).map(str::to_owned));
      inputs.sort_unstable();
      inputs.dedup();
      let manifest = Manifest::build(cache, &mut self.store, &inputs, &[], &[])?;
      let key = manifest.result_key(request)?;
      Record {
         status:      0,
         diagnostics: result.stderr.clone(),
         depfile:     None,
         link_deps:   None,
         artifacts:   vec![Artifact {
            name:   "makefile".to_owned(),
            mode:   None,
            bytes:  self.store.stored_text(&text)?.into_bytes(),
            settle: false,
         }],
      }
      .put(cache, &key)?;
      manifest.publish(cache, request)?;
      Ok(true)
   }
}

fn scanned_sources(arguments: &[String]) -> impl Iterator<Item = &str> {
   arguments
      .iter()
      .filter(|argument| {
         !argument.starts_with('-') && SOURCES.iter().any(|suffix| argument.ends_with(suffix))
      })
      .map(String::as_str)
}

fn served_scan(
   compiler: &str,
   arguments: &[String],
   socket: &Path,
   started: Instant,
) -> io::Result<Option<i32>> {
   let Ok(mut client) = Client::connect(socket) else {
      return Ok(None);
   };
   let makefile = arguments
      .windows(2)
      .find(|pair| pair[0] == "-dep-makefile")
      .map_or_else(|| "Makefile".to_owned(), |pair| pair[1].clone());
   let mut store = Store::new();
   store.learn_random(arguments);
   let info = Command::new(compiler).arg("--info").output()?;
   if !info.status.success() {
      return Ok(None);
   }
   let mut hasher = Hasher::new();
   field(&mut hasher, "scan");
   field(&mut hasher, store.tool_identity(&mut client, compiler)?);
   field(
      &mut hasher,
      store.key(&String::from_utf8(info.stdout).map_err(io::Error::other)?),
   );
   field(&mut hasher, store.key(utf8(&current_dir()?)?));
   for argument in arguments {
      field(&mut hasher, store.key(argument));
   }
   for name in ["GHC_PACKAGE_PATH", "GHC_ENVIRONMENT", "LANG", "LC_ALL"] {
      field(&mut hasher, name);
      field(&mut hasher, var(name).unwrap_or_default());
   }
   for source in scanned_sources(arguments) {
      field(&mut hasher, store.key(source));
      field(&mut hasher, read(source)?);
   }
   let request = Key::digest(Namespace::Haskell, &Digest::from(hasher.finalize()));
   let mut scan = Scan {
      compiler,
      arguments,
      makefile,
      store,
   };
   serve(&mut scan, &mut client, &request, "dependency scan", started).map(Some)
}

/// The daemon, request key and dependency list, or None after saying why
/// this compile runs uncached.
fn prepare(
   store: &Store,
   compiler: &str,
   arguments: &[String],
   invocation: &Invocation,
   socket: &Path,
) -> Option<(Client, Key, Vec<String>)> {
   let mut client = Client::connect(socket)
      .map_err(|error| diagnostic(format_args!("mummer Haskell cache unavailable, {error}")))
      .ok()?;
   let dependencies = invocation
      .dependencies()
      .map_err(|error| diagnostic(format_args!("mummer cannot find Haskell inputs, {error}")))
      .ok()?;
   let key = request_key(store, &mut client, compiler, arguments, invocation)
      .map_err(|error| {
         diagnostic(format_args!(
            "mummer cannot identify Haskell inputs, {error}"
         ));
      })
      .ok()?;
   Some((client, key, dependencies))
}

fn request_key(
   store: &Store,
   cache: &mut Client,
   compiler: &str,
   arguments: &[String],
   invocation: &Invocation,
) -> io::Result<Key> {
   let info = Command::new(compiler).arg("--info").output()?;
   if !info.status.success() {
      return Err(io::Error::other(format!("{compiler} --info failed")));
   }
   let mut hasher = Hasher::new();
   field(&mut hasher, store.tool_identity(cache, compiler)?);
   field(
      &mut hasher,
      store.key(&String::from_utf8(info.stdout).map_err(io::Error::other)?),
   );
   field(&mut hasher, store.key(utf8(&current_dir()?)?));
   for argument in arguments {
      field(&mut hasher, store.key(argument));
   }
   for name in [
      "GHC_PACKAGE_PATH",
      "GHC_ENVIRONMENT",
      "SOURCE_DATE_EPOCH",
      "LANG",
      "LC_ALL",
   ] {
      field(&mut hasher, name);
      field(&mut hasher, var(name).unwrap_or_default());
   }
   field(&mut hasher, "source");
   let mut bytes = read(&invocation.source)?;
   store.mask_output(&mut bytes, false);
   field(&mut hasher, bytes);
   Ok(Key::digest(
      Namespace::Haskell,
      &Digest::from(hasher.finalize()),
   ))
}

/// Modification times, so only what the compile wrote is stored, whatever
/// the output flags made of the paths predicted from `-o`.
fn modified(paths: &[String]) -> Vec<Option<SystemTime>> {
   paths
      .iter()
      .map(|path| metadata(path).and_then(|meta| meta.modified()).ok())
      .collect()
}

fn replay(cache: &mut Client, store: &Store, key: &Digest) -> Result<(), Miss> {
   let record = Record::get(cache, key)?;
   for mut artifact in record.artifacts {
      store.mask_output(&mut artifact.bytes, true);
      write_file(Path::new(&artifact.name), &artifact.bytes, artifact.mode)?;
      remember_written(cache, store, Path::new(&artifact.name), artifact.bytes)?;
   }
   io::stderr().lock().write_all(&record.diagnostics)?;
   Ok(())
}
