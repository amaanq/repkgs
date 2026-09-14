use std::{
   io::{
      self,
      Write as _,
   },
   path::PathBuf,
   process::{
      Command,
      Stdio,
   },
};

use crate::store::on_path;

pub fn tool(name: &str) -> io::Result<PathBuf> {
   let path = on_path(name);
   if !path.is_absolute() {
      return Err(io::Error::other(format!("{name} not on PATH")));
   }
   Ok(path)
}

pub fn run(program: &str, args: &[String], env: &[(&str, String)]) -> io::Result<()> {
   drop(writeln!(
      io::stderr().lock(),
      "+ {program} {}",
      args.join(" ")
   ));
   let status = Command::new(program)
      .args(args)
      .envs(env.iter().cloned())
      .status()?;
   if !status.success() {
      return Err(io::Error::other(format!(
         "{program} failed with {}",
         status.code().unwrap_or(-1)
      )));
   }
   Ok(())
}

pub fn capture(program: &str, args: &[String]) -> io::Result<String> {
   drop(writeln!(
      io::stderr().lock(),
      "+ {program} {}",
      args.join(" ")
   ));
   let output = Command::new(program)
      .args(args)
      .stderr(Stdio::inherit())
      .output()?;
   if !output.status.success() {
      return Err(io::Error::other(format!(
         "{program} failed with {}",
         output.status.code().unwrap_or(-1)
      )));
   }
   Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

pub fn unknown(name: &str, verb: &str) -> io::Error {
   io::Error::other(format!("{name} has no {verb} phase"))
}
