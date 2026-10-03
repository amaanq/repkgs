//! The nix and host commands plumb runs.

use std::{
   ffi::OsStr,
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

use base64::{
   Engine as _,
   engine::general_purpose::STANDARD,
};
use serde::Deserialize;
use serde_json::Value;

pub struct Finished {
   pub status: i32,
   pub stdout: String,
   pub stderr: String,
}

/// Runs a command to completion, stdout and stderr captured.
pub fn capture<Program, Arg, Args>(program: Program, args: Args) -> io::Result<Finished>
where
   Program: AsRef<OsStr>,
   Arg: AsRef<OsStr>,
   Args: IntoIterator<Item = Arg>,
{
   let output = Command::new(program)
      .args(args)
      .stdin(Stdio::null())
      .output()?;
   Ok(Finished {
      status: output.status.code().unwrap_or(-1),
      stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
      stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
   })
}

/// stdout of a command that must succeed, its stderr forwarded.
pub fn succeed<Program, Arg, Args>(program: Program, args: Args) -> io::Result<String>
where
   Program: AsRef<OsStr> + Copy,
   Arg: AsRef<OsStr>,
   Args: IntoIterator<Item = Arg>,
{
   let finished = capture(program, args)?;
   if finished.status != 0_i32 {
      return Err(io::Error::other(format!(
         "{} failed with {}\n{}",
         Path::new(program.as_ref()).display(),
         finished.status,
         finished.stderr.trim()
      )));
   }
   Ok(finished.stdout)
}

pub fn say(message: &str) {
   drop(writeln!(io::stderr().lock(), "{message}"));
}

pub fn build(root: &Path, attr: &str) -> io::Result<String> {
   let out = succeed("nix-build", [
      root.as_os_str(),
      OsStr::new("-A"),
      OsStr::new(attr),
      OsStr::new("--no-out-link"),
   ])?;
   Ok(out.trim().to_owned())
}

pub fn eval_raw(root: &Path, attr: &str) -> io::Result<String> {
   let out = succeed("nix", [
      OsStr::new("eval"),
      OsStr::new("--raw"),
      OsStr::new("-f"),
      root.as_os_str(),
      OsStr::new(attr),
   ])?;
   Ok(out.trim().to_owned())
}

pub fn eval_json(root: &Path, attr: &str) -> io::Result<Value> {
   let out = succeed("nix", [
      OsStr::new("eval"),
      OsStr::new("--json"),
      OsStr::new("-f"),
      root.as_os_str(),
      OsStr::new(attr),
   ])?;
   Ok(serde_json::from_str(&out)?)
}

#[derive(Deserialize)]
pub struct Prefetched {
   pub hash:       String,
   #[serde(rename = "storePath")]
   pub store_path: String,
}

pub fn prefetch_file(url: &str) -> io::Result<Prefetched> {
   let out = succeed("nix", [
      "store",
      "prefetch-file",
      "--json",
      "--hash-type",
      "sha256",
      url,
   ])?;
   Ok(serde_json::from_str(&out)?)
}

/// The SRI hash of a store-shaped tree.
pub fn hash_path(path: &Path) -> io::Result<String> {
   let out = succeed("nix", [
      OsStr::new("hash"),
      OsStr::new("path"),
      OsStr::new("--sri"),
      OsStr::new("--type"),
      OsStr::new("sha256"),
      path.as_os_str(),
   ])?;
   Ok(out.trim().to_owned())
}

/// A hex sha256 as `sha256-<base64>`.
pub fn sri(hex: &str) -> io::Result<String> {
   if hex.len() != 64 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
      return Err(io::Error::other(format!("not a sha256 hex digest: {hex}")));
   }
   let bytes: Vec<u8> = (0..hex.len())
      .step_by(2)
      .map(|index| u8::from_str_radix(&hex[index..index + 2], 16).map_err(io::Error::other))
      .collect::<io::Result<_>>()?;
   Ok(format!("sha256-{}", STANDARD.encode(bytes)))
}

/// Metric units the way nu's `into filesize` prints them.
pub fn filesize(bytes: u64) -> String {
   const UNITS: [(&str, u64); 4] = [
      ("GB", 1_000_000_000),
      ("MB", 1_000_000),
      ("kB", 1_000),
      ("B", 1),
   ];
   for (unit, scale) in UNITS {
      if bytes >= scale {
         let whole = bytes / scale;
         let tenths = (bytes % scale) * 10 / scale;
         return if scale == 1 {
            format!("{whole} {unit}")
         } else {
            format!("{whole}.{tenths} {unit}")
         };
      }
   }
   "0 B".to_owned()
}
