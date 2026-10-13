use std::{
   collections::BTreeMap,
   env::{
      remove_var,
      set_var,
   },
   io::{
      self,
      Write as _,
   },
   path::{
      Path,
      PathBuf,
   },
   process::Command,
};

use mummer::{
   files::{
      sorted_entries,
      utf8,
   },
   process::{
      Checked as _,
      strict,
   },
   store::on_path,
};
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
   Unset,
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

   pub fn unset(&mut self, name: &str) {
      self.0.insert(name.to_owned(), Setting::Unset);
   }

   /// into this process, lists joined the way nu exports PATH.
   pub fn apply(&self) {
      for (name, setting) in &self.0 {
         match *setting {
            Setting::Text(ref value) => export(name, value),
            Setting::List(ref values) => export(name, &values.join(":")),
            Setting::Unset => {
               // SAFETY: the build runs single-threaded between phases
               unsafe { remove_var(name) }
            },
         }
      }
   }
}

pub fn export(name: &str, value: &str) {
   // SAFETY: the build runs single-threaded between phases
   unsafe { set_var(name, value) }
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

pub fn file_names(directory: &Path) -> io::Result<Vec<String>> {
   sorted_entries(directory)?
      .iter()
      .filter_map(|path| path.file_name())
      .map(|name| Ok(utf8(Path::new(name))?.to_owned()))
      .collect()
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
   Command::new(program)
      .args(args)
      .envs(env.iter().copied())
      .checked()
}

pub fn capture(program: &str, args: &[&str]) -> io::Result<String> {
   echo(program, args);
   let output = Command::new(program).args(args).checked_output()?;
   Ok(strict(&output, program)?.to_owned())
}

pub fn upsert<Value>(defs: &mut Vec<(String, Value)>, name: &str, value: Value) {
   match defs.iter_mut().find(|entry| entry.0 == name) {
      Some(entry) => entry.1 = value,
      None => defs.push((name.to_owned(), value)),
   }
}

pub fn unknown(name: &str, verb: &str) -> io::Error {
   io::Error::other(format!("{name} has no {verb} phase"))
}
