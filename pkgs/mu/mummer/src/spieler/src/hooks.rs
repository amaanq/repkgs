//! `update.nu` beside a recipe.toml may export `resolve`, `files` or
//! `verify` to replace that stage for its package.
//!
//! It runs in its own nu with this binary's directory on PATH (so
//! `^spieler versions` and `^spieler get` serve it), gets its record as JSON
//! in `$env.SPIELER_ARG` and answers in JSON on stdout. stderr is shown.

use std::{
   env::{
      current_exe,
      var_os,
   },
   ffi::OsString,
   io::{
      self,
      Write as _,
   },
   path::Path,
   process::{
      Command,
      Stdio,
   },
};

use serde::Serialize;
use serde_json::Value;

const STAGES: [&str; 3] = ["resolve", "files", "verify"];

fn run_nu(code: &str, what: &str, arg: &str) -> io::Result<Value> {
   let mut path = OsString::new();
   if let Ok(exe) = current_exe()
      && let Some(dir) = exe.parent()
   {
      path.push(dir);
      path.push(":");
   }
   path.push(var_os("PATH").unwrap_or_default());
   let output = Command::new("nu")
      .args(["--no-config-file", "-c", code])
      .env("SPIELER_ARG", arg)
      .env("PATH", path)
      .stdin(Stdio::null())
      .output()?;
   let stderr = String::from_utf8_lossy(&output.stderr);
   if !output.status.success() {
      return Err(io::Error::other(format!("{what}: {}", stderr.trim())));
   }
   if !stderr.trim().is_empty() {
      drop(writeln!(io::stderr().lock(), "{}", stderr.trim()));
   }
   serde_json::from_slice(&output.stdout)
      .map_err(|error| io::Error::other(format!("{what}: not JSON, {error}")))
}

/// The stages a hook file exports, anything else is an error.
pub fn exports(file: &Path) -> io::Result<Vec<String>> {
   let code = format!(
      "use {}; scope modules | where name == 'update' | first | get commands.name | to json",
      file.display()
   );
   let listed = run_nu(&code, &file.to_string_lossy(), "null")?;
   let names: Vec<String> = listed
      .as_array()
      .map(|names| {
         names
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect()
      })
      .unwrap_or_default();
   let bad: Vec<&str> = names
      .iter()
      .map(String::as_str)
      .filter(|name| !STAGES.contains(name))
      .collect();
   if !bad.is_empty() {
      return Err(io::Error::other(format!(
         "{}: unknown exports {}. Known: {}",
         file.display(),
         bad.join(", "),
         STAGES.join(" ")
      )));
   }
   Ok(names)
}

/// One stage of a hook over its record.
pub fn run<Record>(file: &Path, stage: &str, record: &Record) -> io::Result<Value>
where
   Record: Serialize,
{
   let code = format!(
      "use {}; update {stage} ($env.SPIELER_ARG | from json) | to json",
      file.display()
   );
   run_nu(
      &code,
      &format!("{} {stage}", file.display()),
      &serde_json::to_string(record)?,
   )
}
