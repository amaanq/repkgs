use std::{
   env::{
      current_dir,
      var,
   },
   ffi::OsStr,
   fs::{
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
   },
   time::Instant,
};

use blake3::Hasher;
use jig::{
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
use wire::{
   client::Client,
   digest,
};

use crate::manifest::{
   Manifest,
   Miss,
   lookup,
   parse_depfile,
};

struct Invocation {
   key_args:  Vec<String>,
   source:    String,
   out_dir:   PathBuf,
   name:      String,
   suffix:    String,
   externs:   Vec<String>,
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
      invocation.cacheable &= !invocation.query
         && sources == 1_i32
         && library
         && dep_info
         && !invocation.out_dir.as_os_str().is_empty()
         && !invocation.name.is_empty();
      invocation
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
         diagnostic(format_args!("jig Rust cache unavailable, {error}"));
         None
      },
   };
   let request = if cache.is_some() {
      match request_key(&store, compiler, &invocation) {
         Ok(key) => Some(key),
         Err(error) => {
            diagnostic(format_args!("jig cannot identify Rust inputs, {error}"));
            None
         },
      }
   } else {
      None
   };
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
      .and_then(|result| replay(client, &invocation.out_dir, &invocation.suffix, &result));
   let miss = match hit {
      Ok(()) => {
         log("rustc", "cached", &label, started);
         return Ok(0);
      },
      Err(Miss::Failed(error)) => {
         diagnostic(format_args!(
            "jig Rust cache replay failed, compiling again, {error}"
         ));
         Miss::Failed(error)
      },
      Err(miss) => miss,
   };
   let explained = format!("{label} {miss}");
   let result = run(
      OsStr::new(compiler),
      arguments,
      Slot::acquire(client).ok().flatten(),
      true,
   )?;
   io::stderr().lock().write_all(&result.stderr)?;
   if result.status != 0_i32 {
      log("rustc", "failed", &explained, started);
      return Ok(result.status);
   }
   let stored = store_result(client, &mut store, key, &invocation, &result.stderr);
   let outcome = match stored {
      Ok(()) => "compiled",
      Err(error) => {
         diagnostic(format_args!("jig could not store Rust result, {error}"));
         "compiled-unstored"
      },
   };
   log("rustc", outcome, &explained, started);
   Ok(0)
}

fn request_key(store: &Store, compiler: &str, invocation: &Invocation) -> io::Result<String> {
   let mut hasher = Hasher::new();
   field(&mut hasher, store.tool_id(compiler)?);
   field(&mut hasher, store.key(&current_dir()?.to_string_lossy()));
   for argument in &invocation.key_args {
      field(&mut hasher, store.key(argument));
   }
   field(&mut hasher, read(&invocation.source)?);
   for external in &invocation.externs {
      field(&mut hasher, store.input_id(external)?);
   }
   let path = on_path(compiler);
   let root = path
      .parent()
      .and_then(Path::parent)
      .ok_or_else(|| io::Error::other("rustc has no installation root"))?;
   let mut libraries = Vec::new();
   for target in read_dir(root.join("lib/rustlib"))? {
      let directory = target?.path().join("lib");
      if !directory.is_dir() {
         continue;
      }
      for library in read_dir(directory)? {
         let entry = library?;
         let name = entry.file_name();
         let text = name.to_string_lossy();
         if (text.starts_with("libstd-") || text.starts_with("libcore-")) && text.ends_with(".rlib")
         {
            libraries.push(entry.path());
         }
      }
   }
   if libraries.is_empty() {
      return Err(io::Error::other("rustc standard libraries were not found"));
   }
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
   inputs.push(invocation.source.clone());
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
   let manifest = Manifest::build(cache, store, &inputs, &variables)?;
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
      let bytes = read(&output)?;
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

fn replay(cache: &mut Client, directory: &Path, suffix: &str, key: &str) -> Result<(), Miss> {
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
   for (artifact, bytes) in artifacts.iter().zip(values) {
      write_file(
         &directory.join(stem(&artifact.name, "@", suffix)),
         &bytes,
         Some(artifact.mode),
      )?;
   }
   io::stderr().lock().write_all(&diagnostics)?;
   Ok(())
}
