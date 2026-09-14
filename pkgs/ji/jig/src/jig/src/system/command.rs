use std::{
   collections::BTreeMap,
   io::{
      self,
      Write as _,
   },
   path::{
      Path,
      PathBuf,
   },
   process::{
      Command,
      Stdio,
   },
};

use serde::Serialize;

use crate::{
   attrs::{
      Attrs,
      Ctx,
   },
   exports::exports_of,
   store::on_path,
};

#[derive(Serialize)]
#[serde(untagged)]
pub enum Setting {
   Text(String),
   List(Vec<String>),
}

pub type Env = BTreeMap<String, Setting>;

pub fn dep_root(ctx: &Ctx, name: &str, why: &str) -> io::Result<String> {
   ctx.deps
      .iter()
      .find(|dep| dep.name == name)
      .map(|dep| dep.root.clone())
      .ok_or_else(|| io::Error::other(format!("{why}: pkgs.{name} must be in dependencies")))
}

pub fn tool_root(attrs: &Attrs, name: &str) -> io::Result<String> {
   for root in &attrs.build_dependencies {
      if exports_of(Path::new(root))?.name == name {
         return Ok(root.clone());
      }
   }
   Err(io::Error::other(format!(
      "buildPkgs.{name} must be in buildDependencies"
   )))
}

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
