use std::{
   collections::BTreeMap,
   env::var,
   fs::{
      read_to_string,
      write,
   },
   io,
   path::Path,
};

use serde::{
   Deserialize,
   Serialize,
};
use serde_json::{
   Map,
   Value,
};

use crate::{
   exports::Exports,
   store::is_store_path,
};

#[derive(Deserialize)]
pub struct Attrs {
   #[serde(rename = "buildDependencies")]
   pub build_dependencies: Vec<String>,
   pub dependencies:       Vec<String>,
   #[serde(rename = "CONFIG_SITE")]
   pub config_site:        String,
   pub hardening:          Hardening,
   pub spec:               Value,
   #[serde(default)]
   pub src:                String,
   #[serde(default)]
   pub patches:            Vec<String>,
   pub outputs:            Outputs,
   #[serde(default)]
   pub package:            String,
   #[serde(default)]
   pub platform:           Value,
}

#[derive(Deserialize)]
pub struct Outputs {
   pub out:   String,
   pub debug: String,
   #[serde(default)]
   pub tree:  Option<String>,
}

#[derive(Deserialize)]
pub struct Hardening {
   pub flags:   BTreeMap<String, Vec<String>>,
   pub cxx:     Vec<String>,
   pub link:    Vec<String>,
   pub enabled: BTreeMap<String, bool>,
}

impl Attrs {
   pub fn load() -> io::Result<Self> {
      let path = var("NIX_ATTRS_JSON_FILE")
         .map_err(|_unset| io::Error::other("NIX_ATTRS_JSON_FILE is not set"))?;
      Ok(serde_json::from_str(&read_to_string(path)?)?)
   }

   pub fn spec(&self) -> io::Result<Spec> {
      Ok(serde_json::from_value(self.spec.clone())?)
   }
}

#[derive(Deserialize)]
pub struct Spec {
   pub name:     String,
   #[serde(default)]
   pub version:  String,
   pub debug:    bool,
   #[serde(default)]
   pub prebuilt: Value,
   #[serde(default)]
   pub root:     Option<String>,
   #[serde(default)]
   pub bin:      Option<Vec<String>>,
   #[serde(default)]
   pub install:  Map<String, Value>,
   #[serde(default)]
   pub links:    BTreeMap<String, String>,
   #[serde(default)]
   pub tests:    Tests,
   #[serde(default)]
   pub exports:  Value,
   #[serde(default)]
   pub cc:       Option<Cc>,
   #[serde(default)]
   pub env:      BTreeMap<String, Value>,
   #[serde(default)]
   pub uses:     Vec<String>,
   #[serde(flatten)]
   pub systems:  Map<String, Value>,
}

#[derive(Deserialize)]
pub struct Tests {
   #[serde(default = "enabled")]
   pub run:     bool,
   #[serde(default)]
   pub version: Value,
   #[serde(default)]
   pub dlopen:  Vec<String>,
}

impl Default for Tests {
   fn default() -> Self {
      Self {
         run:     true,
         version: Value::Null,
         dlopen:  Vec::new(),
      }
   }
}

const fn enabled() -> bool {
   true
}

#[derive(Deserialize, Default)]
pub struct Cc {
   #[serde(default)]
   pub hardening: Option<Value>,
   #[serde(default)]
   pub cflags:    Vec<String>,
   #[serde(default)]
   pub cxxflags:  Vec<String>,
   #[serde(default)]
   pub ldflags:   Vec<String>,
}

impl Spec {
   pub fn lock_trees(&self) -> impl Iterator<Item = &str> {
      self
         .uses
         .iter()
         .filter_map(|name| self.systems.get(name)?.get("deps")?.as_str())
         .filter(|tree| is_store_path(tree))
   }

   pub fn prebuilt_is(&self, wanted: &Value) -> bool {
      self.prebuilt == *wanted
   }
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Ctx {
   pub spec:      Value,
   pub out:       String,
   pub dest:      String,
   pub deps:      Vec<Exports>,
   pub roots:     Vec<String>,
   pub njobs:     usize,
   pub src:       String,
   pub build:     String,
   pub platform:  Value,
   pub tests_run: bool,
   pub cache:     bool,
}

impl Ctx {
   pub fn save(&self, top: &str) -> io::Result<()> {
      write(Path::new(top).join("ctx.json"), serde_json::to_vec(self)?)
   }

   pub fn load(top: &str) -> io::Result<Self> {
      Ok(serde_json::from_str(&read_to_string(
         Path::new(top).join("ctx.json"),
      )?)?)
   }
}

pub fn text<'value>(value: &'value Value, name: &str) -> &'value str {
   value.get(name).and_then(Value::as_str).unwrap_or_default()
}

pub fn flag(value: &Value, name: &str) -> bool {
   value.get(name).and_then(Value::as_bool).unwrap_or(false)
}

pub fn words(value: &Value, name: &str) -> Vec<String> {
   value
      .get(name)
      .and_then(Value::as_array)
      .map(|list| {
         list
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect()
      })
      .unwrap_or_default()
}

pub fn build_top() -> io::Result<String> {
   var("NIX_BUILD_TOP").map_err(|_unset| io::Error::other("NIX_BUILD_TOP is not set"))
}
