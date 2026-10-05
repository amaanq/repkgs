use std::{
   collections::BTreeMap,
   env::set_var,
   ffi::OsStr,
   fs::read_to_string,
   io::{
      self,
      Write as _,
   },
   path::Path,
   process::Command,
};

use mummer::{
   attrs::load,
   store::STORE_DIR,
};
use serde_json::Value;

use crate::bootstrap::{
   apple_sdk::AppleSdk,
   cc::Cc,
   cmake::Cmake,
   compiler_rt::CompilerRt,
   dlaudit::Dlaudit,
   glibc::Glibc,
   launch::Launch,
   linux::Linux,
   llvm::Llvm,
   mingw::Mingw,
   mummer::Mummer,
   musl::Musl,
   runtimes::Runtimes,
   stage::{
      Stage,
      execute,
   },
   sysroot::Sysroot,
   vcruntime::Vcruntime,
};

pub trait Recipe: Sync {
   fn name(&self) -> &'static str;
   fn run(&self, stage: &Stage) -> io::Result<()>;
}

const RECIPES: &[&dyn Recipe] = &[
   &Mummer,
   &Launch,
   &Dlaudit,
   &Linux,
   &Sysroot,
   &AppleSdk,
   &Musl,
   &Glibc,
   &Mingw,
   &CompilerRt,
   &Runtimes,
   &Cc,
   &Cmake,
   &Llvm,
   &Vcruntime,
];

fn recipe(name: &str) -> io::Result<&'static dyn Recipe> {
   RECIPES
      .iter()
      .copied()
      .find(|candidate| candidate.name() == name)
      .ok_or_else(|| io::Error::other(format!("no bootstrap recipe {name}")))
}

fn text_of(value: &Value) -> Option<String> {
   match *value {
      Value::String(ref text) => Some(text.clone()),
      Value::Bool(flag) => Some(flag.to_string()),
      Value::Number(ref number) => Some(number.to_string()),
      Value::Array(ref items) => {
         Some(
            items
               .iter()
               .filter_map(text_of)
               .collect::<Vec<String>>()
               .join(" "),
         )
      },
      Value::Null | Value::Object(_) => None,
   }
}

/// the structured attrs as environment for the recipe's children, lists
/// space-joined, and the compile cache's roots, every store path handed to
/// the stage plus what its sysroot links to.
fn export(stage: &Stage) -> io::Result<()> {
   let attrs = load::<BTreeMap<String, Value>>()?;
   let mut roots = Vec::new();
   for (key, value) in &attrs {
      if matches!(key.as_str(), "outputs" | "args" | "builder") {
         continue;
      }
      let Some(text) = text_of(value) else {
         continue;
      };
      if matches!(*value, Value::String(_)) && text.contains(&format!("{STORE_DIR}/")) {
         roots.push(text.clone());
      }
      export_var(key, &text);
   }
   if let Some(sysroot) = stage.sysroot.as_deref() {
      match read_to_string(sysroot.join("roots")) {
         Ok(linked) => roots.push(linked),
         Err(error) if error.kind() == io::ErrorKind::NotFound => {},
         Err(error) => return Err(error),
      }
   }
   export_var("out", &stage.out);
   export_var("MUMMER_LOG", stage.build_top.join("mummer.log"));
   export_var("MUMMER_STORE_ROOTS", roots.join(" "));
   Ok(())
}

fn export_var(key: &str, value: impl AsRef<OsStr>) {
   // SAFETY: export runs before the recipe spawns any thread
   unsafe { set_var(key, value) }
}

fn summary(log: &Path) {
   let Ok(text) = read_to_string(log) else {
      return;
   };
   let mut kinds: BTreeMap<&str, usize> = BTreeMap::new();
   for line in text.lines() {
      let Some(outcome) = line.split('\t').nth(1) else {
         continue;
      };
      if outcome != "query" {
         *kinds.entry(outcome).or_default() += 1;
      }
   }
   let parts: Vec<String> = kinds
      .iter()
      .map(|(kind, count)| format!("{kind}={count}"))
      .collect();
   drop(writeln!(
      io::stderr().lock(),
      "== cache: {}",
      parts.join(" ")
   ));
}

/// run one stage: the attrs as environment, the recipe, the compile cache
/// summary, then the group and other write bits off $out since the sandbox
/// umask leaves them set and the daemon rejects that.
pub fn mode(name: &str) -> io::Result<()> {
   let stage = Stage::load()?;
   export(&stage)?;
   recipe(name)?.run(&stage)?;
   summary(&stage.build_top.join("mummer.log"));
   execute(Command::new("chmod").args(["-R", "go-w"]).arg(&stage.out))
}
