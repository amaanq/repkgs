//! LLVM's tablegen runs served from the cache. `RULE_LAUNCH_CUSTOM` puts
//! `mummer tblgenwrap` before every custom command, and a `*-tblgen`
//! call that names its output with `-o` and its depfile with `-d` is cached,
//! anything else runs as it is.

use std::{
   env::current_dir,
   ffi::OsStr,
   fs::{
      create_dir_all,
      metadata,
      read,
      read_to_string,
   },
   io::{
      self,
      Write as _,
   },
   os::unix::{
      fs::PermissionsExt as _,
      process::CommandExt as _,
   },
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
      sorted_entries,
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
use tempfile::Builder;
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

/// One tablegen run, the files it writes beside its output and the depfile
/// naming what it read.
struct Run<'call> {
   program:        &'call str,
   arguments:      &'call [String],
   output:         &'call str,
   depfile:        &'call str,
   directory:      PathBuf,
   name:           &'call str,
   /// `--write-if-changed`, which leaves an output that came out the same
   /// untouched so what includes it does not rebuild.
   keep_unchanged: bool,
   store:          Store,
}

struct Written {
   depfile: String,
   outputs: Vec<Artifact>,
}

impl<'call> Run<'call> {
   fn new(program: &'call str, arguments: &'call [String]) -> Option<Self> {
      let output = value_of(arguments, "-o").filter(|output| *output != "-")?;
      let depfile = value_of(arguments, "-d")?;
      let path = Path::new(output);
      let name = path.file_name()?.to_str()?;
      let directory = path
         .parent()
         .filter(|parent| !parent.as_os_str().is_empty())
         .unwrap_or_else(|| Path::new("."))
         .to_path_buf();
      Some(Self {
         program,
         arguments,
         output,
         depfile,
         directory,
         name,
         keep_unchanged: arguments
            .iter()
            .any(|argument| argument == "--write-if-changed"),
         store: Store::new(),
      })
   }

   fn place(&self, name: &str, bytes: &[u8], mode: Option<u32>) -> io::Result<bool> {
      let path = self.directory.join(name);
      if self.keep_unchanged && read(&path).is_ok_and(|existing| existing == bytes) {
         return Ok(false);
      }
      write_file(&path, bytes, mode)?;
      Ok(true)
   }
}

impl Wrapper for Run<'_> {
   const TOOL: Tool = Tool::Tablegen;
   type Compiled = Option<Written>;

   fn manifests(&mut self) -> &mut Store {
      &mut self.store
   }

   fn replay(&mut self, cache: &mut Client, result: &Digest) -> Result<i32, Miss> {
      let record = Record::get(cache, result)?;
      let complete = record
         .artifacts
         .iter()
         .any(|artifact| artifact.name == self.name);
      let (true, Some(depfile)) = (complete, record.depfile) else {
         return Err(Miss::Failed(io::Error::other(
            "cached tablegen run has no output",
         )));
      };
      for artifact in record.artifacts {
         let mut bytes = artifact.bytes;
         self.store.mask_output(&mut bytes, true);
         if self.place(&artifact.name, &bytes, artifact.mode)? {
            remember_written(
               cache,
               &self.store,
               &self.directory.join(&artifact.name),
               bytes,
            )?;
         }
      }
      write_file(
         Path::new(self.depfile),
         self.store.replayed_text(depfile)?.as_bytes(),
         None,
      )?;
      io::stderr().lock().write_all(&record.diagnostics)?;
      Ok(0_i32)
   }

   /// tablegen writes more than `-o` names, `-gen-register-info` adds
   /// `<stem>Enums.inc` and three more beside it that neither the command line
   /// nor the depfile mention, so a miss runs into a private directory and
   /// keeps whatever lands there.
   fn compile(&mut self, cache: &mut Client) -> io::Result<(RunResult, Option<Written>)> {
      let scratch = Builder::new()
         .prefix(".mummer-tblgen-")
         .tempdir_in(&self.directory)?;
      let staged = scratch.path().join("out");
      create_dir_all(&staged)?;
      let staged_output = staged.join(self.name);
      let staged_depfile = scratch.path().join("depfile");
      let mut arguments = self.arguments.to_vec();
      for (flag, value) in [("-o", &staged_output), ("-d", &staged_depfile)] {
         if let Some(at) = arguments.iter().position(|argument| argument == flag) {
            utf8(value)?.clone_into(&mut arguments[at + 1]);
         }
      }

      let result = run(
         OsStr::new(self.program),
         &arguments,
         Slot::acquire(cache).ok().flatten(),
         &[],
      )?;
      if result.status != 0_i32 {
         return Ok((result, None));
      }

      let text = read_to_string(&staged_depfile)?;
      let Some(dependencies) = text.strip_prefix(utf8(&staged_output)?) else {
         return Err(io::Error::other("tablegen's depfile names another output"));
      };
      let depfile = format!("{}{dependencies}", self.output);
      write_file(Path::new(self.depfile), depfile.as_bytes(), None)?;
      let mut outputs = Vec::new();
      for path in sorted_entries(&staged)? {
         let name = path
            .file_name()
            .map(Path::new)
            .map(utf8)
            .transpose()?
            .ok_or_else(|| io::Error::other("tablegen wrote a nameless file"))?
            .to_owned();
         let bytes = read(&path)?;
         let mode = metadata(&path)?.permissions().mode() & 0o777;
         self.place(&name, &bytes, Some(mode))?;
         outputs.push(Artifact {
            name,
            mode: Some(mode),
            bytes,
            settle: false,
         });
      }
      Ok((result, Some(Written { depfile, outputs })))
   }

   fn store(
      &mut self,
      cache: &mut Client,
      request: &Key,
      result: &RunResult,
      compiled: Option<Written>,
   ) -> io::Result<bool> {
      let Some(Written {
         depfile,
         mut outputs,
      }) = compiled
      else {
         return Ok(false);
      };
      let mut inputs = parse_depfile(&depfile);
      inputs.retain(|path| Path::new(path).is_file());
      inputs.sort_unstable();
      inputs.dedup();
      let manifest = Manifest::build(cache, &mut self.store, &inputs, &[], &[])?;
      let key = manifest.result_key(request)?;
      for output in &mut outputs {
         self.store.mask_output(&mut output.bytes, false);
      }
      Record {
         status:      0,
         diagnostics: result.stderr.clone(),
         depfile:     Some(self.store.stored_text(&depfile)?),
         link_deps:   None,
         artifacts:   outputs,
      }
      .put(cache, &key)?;
      manifest.publish(cache, request)?;
      Ok(true)
   }
}

fn value_of<'call>(arguments: &'call [String], flag: &str) -> Option<&'call str> {
   arguments
      .iter()
      .position(|argument| argument == flag)
      .and_then(|at| arguments.get(at + 1))
      .map(String::as_str)
}

fn request_key(
   store: &Store,
   cache: &mut Client,
   program: &str,
   arguments: &[String],
) -> io::Result<Key> {
   let mut hasher = Hasher::new();
   field(&mut hasher, store.tool_identity(cache, program)?);
   field(&mut hasher, store.key(utf8(&current_dir()?)?));
   for argument in arguments {
      field(&mut hasher, store.key(argument));
   }
   Ok(Key::digest(
      Namespace::Tablegen,
      &Digest::from(hasher.finalize()),
   ))
}

pub fn run_tblgen(args: &[String], socket: &Path) -> io::Result<i32> {
   let Some((program, arguments)) = args.split_first() else {
      return Err(io::Error::other("tblgenwrap requires a command"));
   };
   let tablegen = Path::new(program)
      .file_name()
      .and_then(OsStr::to_str)
      .is_some_and(|name| name.ends_with("-tblgen"));
   if !tablegen {
      return Err(Command::new(program).args(arguments).exec());
   }

   let started = Instant::now();
   let outcome = match Run::new(program, arguments) {
      Some(mut job) => {
         match Client::connect(socket) {
            Ok(mut client) => {
               match request_key(&job.store, &mut client, program, arguments) {
                  Ok(request) => {
                     let label = job.output;
                     return serve(&mut job, &mut client, &request, label, started);
                  },
                  Err(error) => {
                     diagnostic(format_args!(
                        "mummer cannot identify the tablegen run, {error}"
                     ));
                     Outcome::Unkeyed
                  },
               }
            },
            Err(_unavailable) => Outcome::NoDaemon,
         }
      },
      None => Outcome::Uncacheable,
   };
   let result = run_slotted(OsStr::new(program), arguments, Some(socket))?;
   uncached(Tool::Tablegen, outcome, program, arguments, started);
   Ok(result.status)
}
