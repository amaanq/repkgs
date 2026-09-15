use std::{
   collections::BTreeMap,
   env::{
      set_var,
      var,
   },
   fs::read_to_string,
   io::{
      self,
      Write as _,
   },
   path::{
      Path,
      PathBuf,
   },
};

use jig::store::STORE_DIR;
use serde_json::Value;

use crate::bootstrap::{
   apple_sdk::AppleSdk,
   cc::Cc,
   compiler_rt::CompilerRt,
   dlaudit::Dlaudit,
   glibc::Glibc,
   jig::Jig,
   launch::Launch,
   linux::Linux,
   mingw::Mingw,
   musl::Musl,
   runtimes::Runtimes,
   stage::{
      Stage,
      run,
   },
   sysroot::Sysroot,
};

pub trait Recipe: Sync {
   fn name(&self) -> &'static str;
   fn run(&self, stage: &Stage) -> io::Result<()>;
}

const RECIPES: &[&dyn Recipe] = &[
   &Jig,
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
   let path = var("NIX_ATTRS_JSON_FILE")
      .map_err(|_unset| io::Error::other("NIX_ATTRS_JSON_FILE is not set"))?;
   let attrs: BTreeMap<String, Value> = serde_json::from_str(&read_to_string(path)?)?;
   let store = var("NIX_STORE").unwrap_or_else(|_unset| STORE_DIR.to_owned());
   let mut roots = Vec::new();
   for (key, value) in &attrs {
      if matches!(key.as_str(), "outputs" | "args" | "builder") {
         continue;
      }
      let Some(text) = text_of(value) else {
         continue;
      };
      if matches!(*value, Value::String(_)) && text.contains(&format!("{store}/")) {
         roots.push(text.clone());
      }
      export_var(key, &text);
   }
   if let Some(sysroot) = stage.sysroot.as_deref()
      && let Ok(linked) = read_to_string(format!("{sysroot}/roots"))
   {
      roots.push(linked);
   }
   export_var("out", stage.out());
   export_var("JIG_LOG", &format!("{}/jig.log", stage.build_top));
   export_var("JIG_STORE_ROOTS", &roots.join(" "));
   Ok(())
}

fn export_var(key: &str, value: &str) {
   // SAFETY: export runs before the recipe spawns any thread
   unsafe { set_var(key, value) }
}

fn summary(log: &Path) {
   let Ok(text) = read_to_string(log) else {
      return;
   };
   let mut kinds: BTreeMap<&str, usize> = BTreeMap::new();
   for line in text.lines() {
      let kind = line.split(' ').next().unwrap_or_default();
      *kinds.entry(kind).or_default() += 1;
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
   summary(&PathBuf::from(&stage.build_top).join("jig.log"));
   run("chmod", &["-R", "go-w", stage.out()])
}
