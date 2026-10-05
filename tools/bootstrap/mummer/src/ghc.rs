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
      canonicalize,
      metadata,
      read,
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
   },
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
      write_file,
   },
   process::{
      Outcome,
      Slot,
      Tool,
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
   digest::Digest,
   key::{
      Key,
      Namespace,
   },
};

use crate::manifest::{
   Manifest,
   Miss,
   lookup,
};

const SOURCES: [&str; 4] = [".hs", ".lhs", ".hs-boot", ".lhs-boot"];

struct Invocation {
   source:    String,
   output:    String,
   outputs:   Vec<String>,
   required:  usize,
   files:     Vec<String>,
   suffixes:  (String, String),
   cacheable: bool,
}

impl Invocation {
   #[expect(
      clippy::too_many_lines,
      reason = "single-pass ghc flag scan, splitting would scatter invocation state"
   )]
   fn parse(args: &[String]) -> Self {
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
      let mut directories = Vec::new();
      let mut files = Vec::new();
      let mut index = 0;

      while index < args.len() {
         let argument = args[index].as_str();
         let mut value = || {
            index += 1;
            args.get(index).cloned()
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

      let source = if sources.len() == 1 {
         sources.swap_remove(0)
      } else {
         String::new()
      };
      let output = named_output.unwrap_or_default();
      let boot = if source.ends_with("-boot") {
         "-boot"
      } else {
         ""
      };
      let base = output
         .strip_suffix(&format!(".{object_suffix}{boot}"))
         .map(str::to_owned);
      cacheable &= compile
         && !source.is_empty()
         && base.is_some()
         && relative_path(&output).is_ok()
         && directories
            .iter()
            .all(|directory| Path::new(&output).starts_with(directory))
         && interface
            .as_deref()
            .is_none_or(|path| relative_path(path).is_ok());

      let mut outputs = vec![output.clone()];
      let mut dynamic = Vec::new();
      if let Some(stem) = base.as_deref() {
         outputs.push(interface.unwrap_or_else(|| format!("{stem}.{interface_suffix}{boot}")));
         if dynamic_too {
            dynamic = vec![
               format!("{stem}.{dynamic_object}{boot}"),
               format!("{stem}.{dynamic_interface}{boot}"),
            ];
         }
      }
      // ghc writes no dynamic object for a boot interface
      let required = outputs.len() + if boot.is_empty() { dynamic.len() } else { 0 };
      outputs.append(&mut dynamic);
      if let Some(stem) = base {
         outputs.push(format!("{stem}_stub.h"));
      }
      files.retain(|file| *file != source && !outputs.contains(file));

      Self {
         source,
         output,
         outputs,
         required,
         files,
         suffixes: (object_suffix, interface_suffix),
         cacheable,
      }
   }

   /// Everything the object depends on in the nearest `.dependencies` above
   /// it, following interfaces in the same package transitively since hadrian
   /// lists direct imports only.
   fn dependencies(&self) -> io::Result<Vec<String>> {
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

#[derive(Deserialize, Serialize)]
struct Artifact {
   path: String,
   mode: u32,
   hash: Digest,
}

pub fn run_ghc(args: &[String], socket: &Path) -> io::Result<i32> {
   let Some((compiler, arguments)) = args.split_first() else {
      return Err(io::Error::other("ghcwrap requires a compiler"));
   };
   let started = Instant::now();
   let invocation = Invocation::parse(arguments);
   let label = invocation.source.clone();
   let mut store = Store::new();
   let prepared = invocation
      .cacheable
      .then(|| prepare(&store, compiler, arguments, &invocation, socket))
      .flatten();
   let Some((mut client, key, dependencies)) = prepared else {
      let result = run_slotted(OsStr::new(compiler), arguments, Some(socket), false)?;
      let outcome = if invocation.cacheable {
         Outcome::NoDaemon
      } else {
         Outcome::Uncacheable
      };
      log(Tool::Ghc, outcome, &label, started);
      if outcome == Outcome::Uncacheable {
         log_uncached(outcome, arguments);
      }
      return Ok(result.status);
   };

   let miss = match lookup(&mut client, &mut store, &key)
      .and_then(|result| replay(&mut client, &store, &result))
   {
      Ok(()) => {
         log(Tool::Ghc, Outcome::Cached, &label, started);
         return Ok(0);
      },
      Err(Miss::Failed(error)) => {
         diagnostic(format_args!(
            "mummer Haskell cache replay failed, compiling again, {error}"
         ));
         Miss::Failed(error)
      },
      Err(miss) => miss,
   };
   let explained = format!("{label} {miss}");
   let before = modified(&invocation.outputs);
   let result = run(
      OsStr::new(compiler),
      arguments,
      Slot::acquire(&mut client).ok().flatten(),
      true,
      &[],
   )?;
   io::stderr().lock().write_all(&result.stderr)?;
   if result.status != 0_i32 {
      log(Tool::Ghc, Outcome::Failed, &explained, started);
      return Ok(result.status);
   }

   let mut paths = dependencies;
   paths.push(invocation.source.clone());
   paths.extend_from_slice(&invocation.files);
   paths.sort_unstable();
   paths.dedup();
   let stored = store_result(
      &mut client,
      &mut store,
      &key,
      &invocation,
      &paths,
      &before,
      &result.stderr,
   );
   let outcome = match stored {
      Ok(()) => Outcome::Compiled,
      Err(error) => {
         diagnostic(format_args!(
            "mummer could not store Haskell result, {error}"
         ));
         Outcome::CompiledUnstored
      },
   };
   log(Tool::Ghc, outcome, &explained, started);
   Ok(0)
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
   let client = Client::connect(socket)
      .map_err(|error| diagnostic(format_args!("mummer Haskell cache unavailable, {error}")))
      .ok()?;
   let dependencies = invocation
      .dependencies()
      .map_err(|error| diagnostic(format_args!("mummer cannot find Haskell inputs, {error}")))
      .ok()?;
   let key = request_key(store, compiler, arguments, invocation)
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
   compiler: &str,
   arguments: &[String],
   invocation: &Invocation,
) -> io::Result<Key> {
   let info = Command::new(compiler).arg("--info").output()?;
   if !info.status.success() {
      return Err(io::Error::other(format!("{compiler} --info failed")));
   }
   let real = canonicalize(on_path(compiler))?;
   let mut hasher = Hasher::new();
   field(
      &mut hasher,
      store.input_id(&real.to_string_lossy())?.to_string(),
   );
   field(
      &mut hasher,
      store.key(&String::from_utf8_lossy(&info.stdout)),
   );
   field(&mut hasher, store.key(&current_dir()?.to_string_lossy()));
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

fn store_result(
   cache: &mut Client,
   store: &mut Store,
   request: &Key,
   invocation: &Invocation,
   inputs: &[String],
   before: &[Option<SystemTime>],
   diagnostics: &[u8],
) -> io::Result<()> {
   let after = modified(&invocation.outputs);
   let manifest = Manifest::build(cache, store, inputs, &[], &[])?;
   let key = manifest.result_key(request)?;
   let mut artifacts = Vec::new();

   for (position, path) in invocation.outputs.iter().enumerate() {
      if after[position].is_none() || after[position] == before[position] {
         if position < invocation.required {
            return Err(io::Error::other(format!(
               "{path} was not written by this compile"
            )));
         }
         continue;
      }
      let mut bytes = read(path).map_err(|error| io::Error::other(format!("{path}: {error}")))?;
      store.mask_output(&mut bytes, false);
      let hash = Digest::of(&bytes);
      cache.put(&Key::digest(Namespace::Blob, &hash), &bytes)?;
      artifacts.push(Artifact {
         path: path.clone(),
         mode: metadata(path)?.permissions().mode() & 0o777,
         hash,
      });
   }

   cache.put(
      &Key::digest(Namespace::Object, &key),
      &serde_json::to_vec(&artifacts)?,
   )?;
   cache.put(&Key::digest(Namespace::Diagnostics, &key), diagnostics)?;
   manifest.publish(cache, request)
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
   let Some(listing) = cache.get(&Key::digest(Namespace::Object, key))? else {
      return Err(Miss::ObjectGone);
   };
   let artifacts = serde_json::from_slice::<Vec<Artifact>>(&listing).map_err(io::Error::from)?;
   let mut values = Vec::new();

   for artifact in &artifacts {
      relative_path(&artifact.path)?;
      if artifact.mode > 0o777 {
         return Err(Miss::Failed(io::Error::other(
            "invalid cached Haskell artifact",
         )));
      }
      let Some(mut bytes) = cache.get(&Key::digest(Namespace::Blob, &artifact.hash))? else {
         return Err(Miss::ObjectGone);
      };
      if Digest::of(&bytes) != artifact.hash {
         return Err(Miss::Failed(io::Error::other(
            "Haskell artifact content hash differs",
         )));
      }
      store.mask_output(&mut bytes, true);
      values.push(bytes);
   }

   let Some(diagnostics) = cache.get(&Key::digest(Namespace::Diagnostics, key))? else {
      return Err(Miss::ObjectGone);
   };
   for (artifact, bytes) in artifacts.iter().zip(values) {
      write_file(&PathBuf::from(&artifact.path), &bytes, Some(artifact.mode))?;
   }
   io::stderr().lock().write_all(&diagnostics)?;
   Ok(())
}
