use std::{
   env::{
      current_dir,
      var,
   },
   ffi::OsStr,
   fs::{
      read,
      read_to_string,
   },
   io::{
      self,
      Write,
   },
   path::{
      Path,
      PathBuf,
   },
   time::Instant,
};

use blake3::Hasher;
use tempfile::Builder;

use crate::{
   cache::CacheClient,
   driver::{
      BinaryFormat,
      Driver,
      expand_response_files,
   },
   files::write_file,
   manifest::{
      Manifest,
      lookup,
      parse_depfile,
   },
   process::{
      diagnostic,
      log,
      run,
   },
   store::{
      Store,
      field,
   },
};

#[derive(Clone, Copy, PartialEq)]
enum Action {
   Compile,
   Assembly,
   Preprocess,
   LinkSource,
   Link,
   Other,
}

struct Invocation {
   args:      Vec<String>,
   source:    String,
   inputs:    Vec<String>,
   output:    PathBuf,
   depfile:   Option<PathBuf>,
   action:    Action,
   query:     bool,
   cacheable: bool,
   stdout:    bool,
}

impl Invocation {
   fn parse(args: Vec<String>) -> Self {
      let mut invocation = Self {
         args,
         source: String::new(),
         inputs: Vec::new(),
         output: PathBuf::new(),
         depfile: None,
         action: Action::Link,
         query: false,
         cacheable: true,
         stdout: false,
      };
      let mut sources = 0;
      let mut wants_deps = false;
      let mut objects = false;
      let mut index = 0;
      while index < invocation.args.len() {
         let arg = &invocation.args[index];
         let next = invocation.args.get(index + 1);
         match arg.as_str() {
            "-c" => invocation.action = Action::Compile,
            "-S" => invocation.action = Action::Assembly,
            "-E" => invocation.action = Action::Preprocess,
            "-o" => {
               if let Some(value) = next {
                  invocation.output = PathBuf::from(value);
                  index += 1;
               }
            },
            "-MF" => {
               if let Some(value) = next {
                  invocation.depfile = Some(PathBuf::from(value));
                  wants_deps = true;
                  index += 1;
               }
            },
            "-MT" | "-MQ" => {
               if next.is_some() {
                  index += 1;
               }
            },
            "-MD" | "-MMD" => wants_deps = true,
            "-M" | "-MM" | "-" | "-v" | "-V" | "--version" | "-###" | "-fsyntax-only" => {
               invocation.query = true;
            },
            "-shared" | "-r" => objects = true,
            "-I" | "-isystem" | "-iquote" | "-idirafter" | "-include" | "-imacros"
            | "-isysroot" | "--sysroot" | "-L" | "-l" | "-x" | "-D" | "-U" | "-target"
            | "--target" => {
               if let Some(value) = next {
                  if arg == "-x" && value.ends_with("-header") {
                     invocation.cacheable = false;
                  }
                  index += 1;
               }
            },
            _ => {
               if arg.starts_with("-o") && arg.len() > 2 {
                  invocation.output = PathBuf::from(&arg[2..]);
               } else if arg.starts_with("-Wp,-MD,") || arg.starts_with("-Wp,-MMD,") {
                  invocation.depfile = arg.splitn(3, ',').nth(2).map(PathBuf::from);
                  wants_deps = true;
               } else if !arg.starts_with('-')
                  && [
                     ".c", ".cc", ".cpp", ".cxx", ".c++", ".C", ".S", ".s", ".sx", ".m",
                  ]
                  .iter()
                  .any(|suffix| arg.ends_with(suffix))
               {
                  invocation.source = arg.to_owned();
                  sources += 1;
               } else if !arg.starts_with('-')
                  && ([".o", ".os", ".lo", ".a", ".so", ".obj"]
                     .iter()
                     .any(|suffix| arg.ends_with(suffix))
                     || arg.contains(".so."))
               {
                  invocation.inputs.push(arg.to_owned());
                  objects = true;
               }
               invocation.query |= arg.starts_with("-print")
                  || arg.starts_with("--print")
                  || arg.starts_with("-dump");
               invocation.cacheable &= !arg.starts_with('@')
                  && !arg.contains(",@")
                  && !arg.ends_with("-header")
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
                     "-include-pch",
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
         index += 1;
      }
      if matches!(
         invocation.action,
         Action::Compile | Action::Assembly | Action::Preprocess
      ) {
         invocation.cacheable &= sources == 1;
         invocation.query |= invocation.action == Action::Preprocess && sources == 0;
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
            (false, 1) => Action::LinkSource,
            (true, 0) if !invocation.inputs.is_empty() => Action::Link,
            _ => Action::Other,
         };
         if invocation.output.as_os_str().is_empty() {
            invocation.output = PathBuf::from("a.out");
         }
      }
      invocation.cacheable &= !invocation.query
         && invocation.output != Path::new("/dev/null")
         && invocation.action != Action::Other
         && !invocation
            .output
            .extension()
            .is_some_and(|extension| extension == "pch" || extension == "gch");
      if wants_deps && invocation.depfile.is_none() {
         invocation.depfile = Some(invocation.output.with_extension("d"));
      }
      invocation
   }

   const fn links(&self) -> bool {
      matches!(self.action, Action::Link | Action::LinkSource)
   }

   fn request_key(&self, store: &Store, compiler: &str) -> io::Result<String> {
      let mut hasher = Hasher::new();
      field(&mut hasher, store.tool_id(compiler)?);
      field(&mut hasher, current_dir()?.as_os_str().as_encoded_bytes());
      for argument in &self.args {
         let mut bytes = argument.as_bytes().to_vec();
         store.mask_output(&mut bytes, false);
         field(&mut hasher, bytes);
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
         let mut bytes = read(&self.source)?;
         store.mask_output(&mut bytes, false);
         field(&mut hasher, bytes);
      }
      for path in &self.inputs {
         field(&mut hasher, store.input_id(path)?);
      }
      Ok(format!("cc-rust/{}", &hasher.finalize().to_hex()[..32]))
   }
}

pub fn run_cc(mode: &str, raw: &[String], socket: &Path) -> io::Result<i32> {
   let started = Instant::now();
   let user = expand_response_files(raw)?;
   let driver = Driver::load()?;
   let mut invocation = Invocation::parse(user);
   if invocation.links() && driver.format() != BinaryFormat::Elf {
      invocation.cacheable = false;
   }
   invocation.args = driver.arguments(mode.contains("++"), &invocation.args);
   let mut store = Store::new();
   let subject = if invocation.source.is_empty() {
      invocation.output.to_string_lossy().into_owned()
   } else {
      invocation.source.clone()
   };
   let connected = if invocation.cacheable {
      match invocation
         .request_key(&store, driver.compiler())
         .and_then(|key| CacheClient::connect(socket).map(|cache| (key, cache)))
      {
         Ok(connection) => Some(connection),
         Err(error) => {
            diagnostic(format_args!("jig compilation cache unavailable, {error}"));
            None
         },
      }
   } else {
      None
   };
   let Some((request, mut cache)) = connected else {
      let slot_socket = (!invocation.query).then_some(socket);
      let result = run(
         OsStr::new(driver.compiler()),
         &invocation.args,
         slot_socket,
         false,
      )?;
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
      return Ok(result.status);
   };
   let hit = lookup(&mut cache, &mut store, &request).and_then(|key| {
      key.map(|result| cached(&mut cache, &store, &invocation, &result))
         .transpose()
         .map(Option::flatten)
   });
   match hit {
      Ok(Some(status)) => {
         log(
            "cc",
            if status == 0 {
               "cached"
            } else {
               "cached-error"
            },
            &subject,
            started,
         );
         return Ok(status);
      },
      Ok(None) => {},
      Err(error) => {
         diagnostic(format_args!(
            "jig cache replay failed, compiling again, {error}"
         ));
      },
   }

   let parent = invocation
      .output
      .parent()
      .filter(|path| !path.as_os_str().is_empty())
      .unwrap_or_else(|| Path::new("."));
   let dependency = Builder::new()
      .prefix(".jig-")
      .suffix(".d")
      .tempfile_in(parent)?;
   let link_dependency = Builder::new()
      .prefix(".jig-")
      .suffix(".link.d")
      .tempfile_in(parent)?;
   let output_capture = if invocation.stdout {
      Some(Builder::new().prefix(".jig-").suffix(".i").tempfile()?)
   } else {
      None
   };
   let output_path = output_capture
      .as_ref()
      .map_or(invocation.output.as_path(), |file| file.path());
   let dependency_path = invocation
      .depfile
      .as_deref()
      .unwrap_or_else(|| dependency.path());
   let mut arguments = invocation.args.clone();
   if invocation.depfile.is_none() && invocation.action != Action::Link {
      arguments.extend([
         "-MD".to_owned(),
         "-MF".to_owned(),
         dependency_path.to_string_lossy().into_owned(),
      ]);
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
      OsStr::new(driver.compiler()),
      &arguments,
      Some(socket),
      true,
   )?;
   io::stderr().lock().write_all(&result.stderr)?;
   if invocation.stdout {
      io::stdout().lock().write_all(&read(output_path)?)?;
   }
   let stored = (|| -> io::Result<bool> {
      let dependencies = if invocation.action == Action::Link {
         String::new()
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
         inputs.extend(
            parse_depfile(&link_text)
               .into_iter()
               .filter(|path| !Path::new(path).starts_with(&temporary)),
         );
      }
      if result.status != 0
         && (invocation.links()
            || inputs.is_empty()
            || String::from_utf8_lossy(&result.stderr).contains("file not found"))
      {
         return Ok(false);
      }
      if !invocation.source.is_empty() {
         inputs.push(invocation.source.clone());
      }
      inputs.extend_from_slice(&invocation.inputs);
      inputs.sort_unstable();
      inputs.dedup();
      let manifest = Manifest::build(&mut cache, &mut store, &inputs, &[])?;
      let key = manifest.result_key(&request)?;
      if result.status == 0 {
         let mut object = read(output_path)?;
         store.mask_output(&mut object, false);
         cache.put(&format!("o/{key}"), &object)?;
         if invocation.depfile.is_some() {
            let mut dep_bytes = store.key(&dependencies).into_bytes();
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
         if result.status == 0 {
            "compiled"
         } else {
            "compiled-error"
         }
      },
      Ok(false) => {
         if result.status == 0 {
            "compiled-unstored"
         } else {
            "failed"
         }
      },
      Err(error) => {
         diagnostic(format_args!("jig could not store compiler result, {error}"));
         if result.status == 0 {
            "compiled-unstored"
         } else {
            "failed"
         }
      },
   };
   log("cc", outcome, &subject, started);
   Ok(result.status)
}

fn cached(
   cache: &mut CacheClient,
   store: &Store,
   invocation: &Invocation,
   key: &str,
) -> io::Result<Option<i32>> {
   let keys = [
      format!("r/{key}"),
      format!("o/{key}"),
      format!("d/{key}"),
      format!("e/{key}"),
   ];
   let mut objects = cache.get_many(&keys)?.into_iter();
   let Some(Some(status_bytes)) = objects.next() else {
      return Ok(None);
   };
   let status = String::from_utf8(status_bytes)
      .map_err(io::Error::other)?
      .parse::<u8>()
      .map_err(io::Error::other)?;
   let object = objects.next().flatten();
   let dependency = objects.next().flatten();
   let Some(Some(mut diagnostics)) = objects.next() else {
      return Ok(None);
   };
   if status == 0 {
      let Some(mut bytes) = object else {
         return Ok(None);
      };
      let dep_text = if invocation.depfile.is_some() {
         let Some(mut dep_bytes) = dependency else {
            return Ok(None);
         };
         store.mask_output(&mut dep_bytes, true);
         let raw = String::from_utf8(dep_bytes).map_err(io::Error::other)?;
         Some(store.resolve_all(&raw))
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
      if let (Some(path), Some(text)) = (&invocation.depfile, dep_text) {
         write_file(path, text.as_bytes(), None)?;
      }
   }
   store.mask_output(&mut diagnostics, true);
   io::stderr().lock().write_all(&diagnostics)?;
   Ok(Some(i32::from(status)))
}
