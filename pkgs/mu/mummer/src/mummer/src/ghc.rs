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
            "-dyno" | "-dynohi" | "-hiedir" | "-stubdir" | "-hidir" | "-odir" | "-dumpdir" => {
               cacheable = false;
            },
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
         && interface
            .as_deref()
            .is_none_or(|path| relative_path(path).is_ok());

      let mut outputs = vec![output.clone()];
      if let Some(stem) = base.as_deref() {
         outputs.push(interface.unwrap_or_else(|| format!("{stem}.{interface_suffix}{boot}")));
         if dynamic_too {
            outputs.push(format!("{stem}.{dynamic_object}{boot}"));
            outputs.push(format!("{stem}.{dynamic_interface}{boot}"));
         }
      }
      let required = outputs.len();
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
   hash: String,
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
         "no-daemon"
      } else {
         "uncacheable"
      };
      log("ghc", outcome, &label, started);
      if outcome == "uncacheable" {
         log_uncached(outcome, arguments);
      }
      return Ok(result.status);
   };

   let miss = match lookup(&mut client, &mut store, &key)
      .and_then(|result| replay(&mut client, &store, &result))
   {
      Ok(()) => {
         log("ghc", "cached", &label, started);
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
   let result = run(
      OsStr::new(compiler),
      arguments,
      Slot::acquire(&mut client).ok().flatten(),
      true,
      &[],
   )?;
   io::stderr().lock().write_all(&result.stderr)?;
   if result.status != 0_i32 {
      log("ghc", "failed", &explained, started);
      return Ok(result.status);
   }
   if !result.deterministic() {
      log("ghc", "compiled-unstored", &explained, started);
      return Ok(0);
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
      &result.stderr,
   );
   let outcome = match stored {
      Ok(()) => "compiled",
      Err(error) => {
         diagnostic(format_args!(
            "mummer could not store Haskell result, {error}"
         ));
         "compiled-unstored"
      },
   };
   log("ghc", outcome, &explained, started);
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
) -> Option<(Client, String, Vec<String>)> {
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
) -> io::Result<String> {
   let info = Command::new(compiler).arg("--info").output()?;
   if !info.status.success() {
      return Err(io::Error::other(format!("{compiler} --info failed")));
   }
   let real = canonicalize(on_path(compiler))?;
   let mut hasher = Hasher::new();
   field(&mut hasher, store.input_id(&real.to_string_lossy())?);
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
   Ok(format!("hs-module/{}", &hasher.finalize().to_hex()[..32]))
}

fn store_result(
   cache: &mut Client,
   store: &mut Store,
   request: &str,
   invocation: &Invocation,
   inputs: &[String],
   diagnostics: &[u8],
) -> io::Result<()> {
   let manifest = Manifest::build(cache, store, inputs, &[], &[])?;
   let key = manifest.result_key(request)?;
   let mut artifacts = Vec::new();

   for (position, path) in invocation.outputs.iter().enumerate() {
      let bytes = match read(path) {
         Ok(mut bytes) => {
            store.mask_output(&mut bytes, false);
            bytes
         },
         Err(error)
            if error.kind() == io::ErrorKind::NotFound && position >= invocation.required =>
         {
            continue;
         },
         Err(error) => return Err(io::Error::other(format!("{path}: {error}"))),
      };
      let hash = digest(&bytes);
      cache.put(&format!("b/{hash}"), &bytes)?;
      artifacts.push(Artifact {
         path: path.clone(),
         mode: metadata(path)?.permissions().mode() & 0o777,
         hash,
      });
   }

   cache.put(&format!("o/{key}"), &serde_json::to_vec(&artifacts)?)?;
   cache.put(&format!("e/{key}"), diagnostics)?;
   manifest.publish(cache, request)
}

fn replay(cache: &mut Client, store: &Store, key: &str) -> Result<(), Miss> {
   let Some(listing) = cache.get(&format!("o/{key}"))? else {
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
      let Some(mut bytes) = cache.get(&format!("b/{}", artifact.hash))? else {
         return Err(Miss::ObjectGone);
      };
      if digest(&bytes) != artifact.hash {
         return Err(Miss::Failed(io::Error::other(
            "Haskell artifact content hash differs",
         )));
      }
      store.mask_output(&mut bytes, true);
      values.push(bytes);
   }

   let Some(diagnostics) = cache.get(&format!("e/{key}"))? else {
      return Err(Miss::ObjectGone);
   };
   for (artifact, bytes) in artifacts.iter().zip(values) {
      write_file(&PathBuf::from(&artifact.path), &bytes, Some(artifact.mode))?;
   }
   io::stderr().lock().write_all(&diagnostics)?;
   Ok(())
}
