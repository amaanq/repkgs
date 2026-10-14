use std::{
   env::{
      current_exe,
      var_os,
   },
   fs::{
      canonicalize,
      read,
      read_to_string,
   },
   io::{
      self,
      Write as _,
   },
   path::Path,
   process::Command,
   time::Instant,
};

use blake3::Hasher;
use misstep::{
   Result,
   bail,
};
use mummer::{
   process::{
      Outcome,
      Slot,
      Tool,
      log,
   },
   store::{
      field,
      in_store,
      is_store_path,
      on_path,
   },
};
use tempfile::NamedTempFile;
use tightrope::{
   client::Client,
   key::{
      Key,
      Namespace,
   },
};

use crate::{
   cc::{
      cached_header,
      jobs,
      uncacheable_marker,
   },
   driver::{
      Driver,
      Language,
   },
   manifest::parse_depfile,
};

/// One autoconf header check, answered by preprocessing it with the
/// compile's flags.
struct Probe<'args> {
   context:  &'args str,
   warnings: &'args str,
   compiler: &'args str,
   expanded: Vec<String>,
}

/// The check's source with unused autoconf defines blanked, what the
/// preprocessor printed and the headers it read.
struct Preprocessed {
   source:       String,
   output:       Vec<u8>,
   dependencies: Vec<String>,
}

impl Probe<'_> {
   /// The flags and environment leave the compiler reading only what its
   /// dependency file reports.
   fn supported(&self) -> io::Result<bool> {
      let compiles_conftest = matches!(
         jobs(self.compiler, &self.expanded)?.as_slice(),
         [job] if job.last().is_some_and(|input| input == "conftest.c")
      );
      Ok(is_store_path(self.compiler)
         && compiles_conftest
         && !self.expanded.iter().any(|arg| reads_unreported_file(arg))
         && ![
            "CCC_OVERRIDE_OPTIONS",
            "CL",
            "_CL_",
            "GCC_EXEC_PREFIX",
            "COMPILER_PATH",
            "SDKROOT",
         ]
         .iter()
         .any(|name| var_os(name).is_some_and(|value| !value.is_empty())))
   }

   fn preprocess(&self, cache: &mut Client) -> Result<Result<Preprocessed, Outcome>> {
      let dependency_file = NamedTempFile::new_in(".")?;
      let slot = Slot::acquire(cache)?;
      let output = Command::new(self.compiler)
         .args(self.expanded.iter().filter(|arg| *arg != "-c"))
         .args(["-E", "-MD", "-MF"])
         .arg(dependency_file.path())
         .args([
            "-MT",
            "probe",
            "-Wunused-macros",
            "-Wno-error=unused-macros",
            "-fno-color-diagnostics",
            "-fno-caret-diagnostics",
         ])
         .env("LC_ALL", "C")
         .output()?;
      drop(slot);
      if output.status.code() == Some(1_i32) {
         return Ok(Err(Outcome::Rejected));
      }
      if !output.status.success() {
         return Ok(Err(Outcome::PreprocessFailed));
      }
      let diagnostics = String::from_utf8(output.stderr)?;
      let Some(source) = normalize_source(&read_to_string("conftest.c")?, &diagnostics) else {
         return Ok(Err(Outcome::Diagnostics));
      };
      let dependencies = parse_depfile(&read_to_string(dependency_file.path())?);
      if dependencies.is_empty() {
         bail!("header preprocessor produced no dependencies")
      }
      Ok(Ok(Preprocessed {
         source,
         output: output.stdout,
         dependencies,
      }))
   }

   fn key(&self, preprocessed: Preprocessed) -> Result<Key> {
      let mut hasher = Hasher::new();
      field(&mut hasher, self.context);
      field(&mut hasher, self.compiler);
      field(&mut hasher, self.warnings);
      for arg in &self.expanded {
         field(&mut hasher, arg);
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
         "LC_CTYPE",
         "LC_MESSAGES",
      ] {
         field(&mut hasher, name);
         field(&mut hasher, serde_json::to_vec(&var_os(name))?);
      }
      field(&mut hasher, preprocessed.source);
      field(&mut hasher, preprocessed.output);
      let source_path = canonicalize("conftest.c")?;
      for dependency in preprocessed.dependencies {
         let path = canonicalize(dependency)?;
         if path == source_path {
            continue;
         }
         field(&mut hasher, path.as_os_str().as_encoded_bytes());
         if !in_store(&path) {
            field(&mut hasher, read(path)?);
         }
      }
      Ok(Key::hex(
         Namespace::HeaderProbe,
         hasher.finalize().as_bytes(),
      ))
   }
}

pub fn lookup(args: &[String], socket: &Path) -> Result<i32> {
   let started = Instant::now();
   let &[ref context, ref warnings, ref compiler, ref raw @ ..] = args else {
      bail!("mummer header-key requires the probe identity, warning policy and compiler command");
   };
   if !Path::new(compiler).file_name().is_some_and(|name| {
      name == "cc" || name == "gcc" || name == "cc-build" || name == "gcc-build"
   }) || canonicalize(on_path(compiler))? != current_exe()?
      || raw.last().is_none_or(|source| source != "conftest.c")
   {
      log(
         Tool::Header,
         Outcome::UnsupportedCompiler,
         compiler,
         started,
      );
      return Ok(1);
   }
   let driver = Driver::load()?;
   let probe = Probe {
      context,
      warnings,
      compiler: driver.cc(),
      expanded: driver.arguments(Language::Cc, raw),
   };
   if !probe.supported()? {
      log(
         Tool::Header,
         Outcome::UnsupportedOptions,
         "conftest.c",
         started,
      );
      return Ok(1);
   }
   let mut cache = Client::connect(socket)?;
   if cached_header(&driver, raw, &mut cache)? {
      log(Tool::Header, Outcome::CompileHit, "conftest.c", started);
      return Ok(0);
   }
   let preprocessed = match probe.preprocess(&mut cache)? {
      Ok(preprocessed) => preprocessed,
      Err(Outcome::Rejected) => {
         log(Tool::Header, Outcome::Rejected, "conftest.c", started);
         return Ok(4);
      },
      Err(outcome) => {
         log(Tool::Header, outcome, "conftest.c", started);
         return Ok(1);
      },
   };
   let key = probe.key(preprocessed)?;
   match cache.get(&key)? {
      Some(bytes) if bytes == b"yes\n" => {
         log(Tool::Header, Outcome::Hit, &key, started);
         Ok(0)
      },
      Some(_) => bail!("invalid cached header result"),
      None => {
         log(Tool::Header, Outcome::Miss, &key, started);
         writeln!(io::stdout().lock(), "{key}")?;
         Ok(3)
      },
   }
}

fn normalize_source(source: &str, diagnostics: &str) -> Option<String> {
   let mut lines = source
      .split('\n')
      .map(str::to_owned)
      .collect::<Vec<String>>();
   for diagnostic in diagnostics.lines() {
      let (location, warning) = diagnostic.split_once(": warning: ")?;
      if !warning.ends_with("macro is not used [-Wunused-macros]") {
         return None;
      }
      let mut parts = location.split(':');
      if parts.next()? != "conftest.c" {
         return None;
      }
      let line_number = parts.next()?.parse::<usize>().ok()?.checked_sub(1)?;
      let column = parts.next()?.parse::<usize>().ok()?;
      if parts.next().is_some() || column != 9 {
         return None;
      }
      let line = lines.get_mut(line_number)?;
      let (macro_name, _) = line.strip_prefix("#define ")?.split_once(' ')?;
      if matches!(
         macro_name,
         "PACKAGE_NAME"
            | "PACKAGE_TARNAME"
            | "PACKAGE_VERSION"
            | "PACKAGE_STRING"
            | "PACKAGE_BUGREPORT"
            | "PACKAGE_URL"
            | "PACKAGE"
            | "VERSION"
      ) && !line.ends_with('\\')
      {
         *line = format!("#define {macro_name} \"\"");
      }
   }
   Some(lines.join("\n"))
}

/// An option that makes the compiler read a file its preprocessor does not
/// report, which the probe key could not cover.
fn reads_unreported_file(arg: &str) -> bool {
   uncacheable_marker(arg)
      || [
         "-fplugin",
         "-fpass-plugin",
         "-fsanitize-ignorelist=",
         "-fsanitize-blacklist=",
         "-fxray-",
         "-fmodule-file",
         "-fprebuilt-module-path",
         "-include-pch",
         "-Xclang",
         "-mllvm",
      ]
      .iter()
      .any(|prefix| arg.starts_with(prefix))
}
