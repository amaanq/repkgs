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
      Write,
   },
   path::Path,
   process::Command,
   time::Instant,
};

use blake3::Hasher;
use eyre::{
   Result,
   bail,
};
use tempfile::NamedTempFile;

use crate::{
   cache::CacheClient,
   driver::Driver,
   manifest::parse_depfile,
   process::{
      Slot,
      log,
   },
   store::{
      field,
      is_store_path,
      on_path,
   },
};

pub fn lookup(args: &[String], socket: &Path) -> Result<i32> {
   let started = Instant::now();
   let [context, warnings, compiler, raw @ ..] = args else {
      bail!("jig header-key requires the probe identity, warning policy and compiler command")
   };
   if !Path::new(compiler).file_name().is_some_and(|name| {
      name == "cc" || name == "gcc" || name == "cc-build" || name == "gcc-build"
   }) || canonicalize(on_path(compiler))? != current_exe()?
      || raw.last().is_none_or(|source| source != "conftest.c")
   {
      log("header", "unsupported-compiler", compiler, started);
      return Ok(1);
   }
   let driver = Driver::load()?;
   let expanded = driver.arguments(false, raw);
   if !is_store_path(driver.compiler())
      || !supported_arguments(&expanded)
      || [
         "CCC_OVERRIDE_OPTIONS",
         "CL",
         "_CL_",
         "GCC_EXEC_PREFIX",
         "COMPILER_PATH",
         "SDKROOT",
      ]
      .iter()
      .any(|name| var_os(name).is_some_and(|value| !value.is_empty()))
   {
      log("header", "unsupported-options", "conftest.c", started);
      return Ok(1);
   }
   let mut cache = CacheClient::connect(socket)?;
   let dependency_file = NamedTempFile::new_in(".")?;
   let slot = Slot::acquire(&mut cache)?;
   let output = Command::new(driver.compiler())
      .args(expanded.iter().filter(|arg| *arg != "-c"))
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
   if !output.status.success() {
      log("header", "preprocess-failed", "conftest.c", started);
      return Ok(1);
   }
   let source = read_to_string("conftest.c")?;
   let diagnostics = String::from_utf8(output.stderr)?;
   let Some(normalized) = normalize_source(&source, &diagnostics) else {
      log("header", "diagnostics", "conftest.c", started);
      return Ok(1);
   };
   let mut hasher = Hasher::new();
   field(&mut hasher, context);
   field(&mut hasher, driver.compiler());
   field(&mut hasher, warnings);
   for arg in &expanded {
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
   field(&mut hasher, normalized);
   field(&mut hasher, output.stdout);
   let source_path = canonicalize("conftest.c")?;
   let dependencies = parse_depfile(&read_to_string(dependency_file.path())?);
   if dependencies.is_empty() {
      bail!("header preprocessor produced no dependencies")
   }
   for dependency in dependencies {
      let path = canonicalize(dependency)?;
      if path == source_path {
         continue;
      }
      field(&mut hasher, path.as_os_str().as_encoded_bytes());
      if !is_store_path(&path.to_string_lossy()) {
         field(&mut hasher, read(path)?);
      }
   }
   let key = format!("probe/header/v1/{}", hasher.finalize().to_hex());
   match cache.get(&key)? {
      Some(bytes) if bytes == b"yes\n" => {
         log("header", "hit", &key, started);
         Ok(0)
      },
      Some(_) => bail!("invalid cached header result"),
      None => {
         log("header", "miss", &key, started);
         writeln!(io::stdout().lock(), "{key}")?;
         Ok(3)
      },
   }
}

fn normalize_source(source: &str, diagnostics: &str) -> Option<String> {
   let mut lines: Vec<_> = source.split('\n').map(str::to_owned).collect();
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

fn supported_arguments(args: &[String]) -> bool {
   let mut arguments = args.iter();
   while let Some(arg) = arguments.next() {
      if matches!(
         arg.as_str(),
         "-I" | "-isystem" | "-iquote" | "-idirafter" | "-D" | "-U"
      ) {
         if arguments.next().is_none() {
            return false;
         }
         continue;
      }
      if matches!(
         arg.as_str(),
         "conftest.c"
            | "-c"
            | "--start-no-unused-arguments"
            | "--end-no-unused-arguments"
            | "-O0"
            | "-O1"
            | "-O2"
            | "-O3"
            | "-Os"
            | "-Oz"
            | "-Og"
            | "-Ofast"
            | "-g"
            | "-g0"
            | "-g1"
            | "-g2"
            | "-g3"
            | "-ggdb"
            | "-fPIC"
            | "-fPIE"
            | "-fpic"
            | "-fpie"
            | "-fno-omit-frame-pointer"
            | "-mno-omit-leaf-frame-pointer"
            | "-fstack-protector"
            | "-fstack-protector-strong"
            | "-fstack-protector-all"
            | "-fno-stack-protector"
            | "-fno-builtin"
            | "-fno-plt"
            | "-fstack-clash-protection"
            | "-fstrict-flex-arrays=1"
            | "-fwrapv"
            | "-ftrivial-auto-var-init=zero"
            | "-fzero-call-used-regs=used-gpr"
            | "-mbranch-protection=standard"
            | "-pthread"
      ) || [
         "-D",
         "-U",
         "-I",
         "-isystem",
         "-iquote",
         "-idirafter",
         "-std=",
         "--target=",
         "--sysroot=",
         "-resource-dir=",
         "-march=",
         "-mcpu=",
         "-mtune=",
         "-B",
         "-ffile-prefix-map=",
         "-fdebug-prefix-map=",
         "-fmacro-prefix-map=",
         "-fcf-protection=",
         "-fvisibility=",
         "-rtlib=",
         "-unwindlib=",
         "-fuse-ld=",
         "--ld-path=",
      ]
      .iter()
      .any(|prefix| arg.starts_with(prefix))
         || (arg.starts_with("-W") && !arg.contains(','))
      {
         continue;
      }
      return false;
   }
   true
}
