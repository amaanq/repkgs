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

use jig::store::on_path;
use serde::Serialize;

use crate::{
   attrs::{
      Attrs,
      Ctx,
   },
   exports::exports_of,
};

#[derive(Serialize)]
#[serde(untagged)]
pub enum Setting {
   Text(String),
   List(Vec<String>),
}

/// what a build system's setup hands the build script to load, PATH as a list.
#[derive(Serialize, Default)]
#[serde(transparent)]
pub struct Env(BTreeMap<String, Setting>);

impl Env {
   pub fn set(&mut self, name: &str, value: String) {
      self.0.insert(name.to_owned(), Setting::Text(value));
   }

   pub fn set_list(&mut self, name: &str, values: Vec<String>) {
      self.0.insert(name.to_owned(), Setting::List(values));
   }
}

impl From<BTreeMap<String, String>> for Env {
   fn from(vars: BTreeMap<String, String>) -> Self {
      Self(
         vars
            .into_iter()
            .map(|(name, value)| (name, Setting::Text(value)))
            .collect(),
      )
   }
}

impl<const COUNT: usize> From<[(&str, String); COUNT]> for Env {
   fn from(vars: [(&str, String); COUNT]) -> Self {
      Self(
         vars
            .into_iter()
            .map(|(name, value)| (name.to_owned(), Setting::Text(value)))
            .collect(),
      )
   }
}

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

pub fn strs(values: &[String]) -> impl Iterator<Item = &str> {
   values.iter().map(String::as_str)
}

fn echo(program: &str, args: &[&str]) {
   drop(writeln!(
      io::stderr().lock(),
      "+ {program} {}",
      args.join(" ")
   ));
}

pub fn run(program: &str, args: &[&str], env: &[(&str, &str)]) -> io::Result<()> {
   echo(program, args);
   let status = Command::new(program)
      .args(args)
      .envs(env.iter().copied())
      .status()?;
   if !status.success() {
      return Err(io::Error::other(format!(
         "{program} failed with {}",
         status.code().unwrap_or(-1)
      )));
   }
   Ok(())
}

pub fn capture(program: &str, args: &[&str]) -> io::Result<String> {
   echo(program, args);
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
