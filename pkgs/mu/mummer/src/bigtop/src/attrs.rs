use std::{
   collections::BTreeMap,
   env::var,
   fs::{
      read_to_string,
      write,
   },
   io,
   path::{
      Path,
      PathBuf,
   },
};

use mummer::store::{
   is_store_path,
   normalize,
};
use serde::{
   Deserialize,
   Serialize,
   Serializer,
   de::DeserializeOwned,
};
use serde_json::{
   Map,
   Value,
};

use crate::{
   exports::Exports,
   hardening::{
      BinaryFormat,
      Choice,
      Hardening,
   },
   schema::{
      OptionType,
      PackagePath,
      Type,
   },
};

#[derive(Deserialize)]
pub struct Attrs {
   pub peanut:             String,
   #[serde(rename = "buildDependencies")]
   pub build_dependencies: Vec<String>,
   pub dependencies:       Vec<String>,
   #[serde(rename = "CONFIG_SITE")]
   pub config_site:        String,
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
   #[serde(rename = "probeInputs", default)]
   pub probe_inputs:       Vec<String>,
   #[serde(default)]
   pub plan:               Plan,
}

/// what nix/package.nix used to render as the build script.
#[derive(Deserialize, Default)]
pub struct Plan {
   #[serde(default)]
   pub prelude:  String,
   #[serde(default)]
   pub setups:   Vec<String>,
   #[serde(default)]
   pub phases:   Vec<Step>,
   #[serde(default)]
   pub separate: bool,
}

#[derive(Deserialize)]
pub struct Step {
   pub name: String,
   pub test: bool,
   #[serde(flatten)]
   pub kind: Kind,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Kind {
   System { system: String, verb: String },
   Nu { body: String },
}

#[derive(Deserialize)]
pub struct Outputs {
   pub out:   String,
   #[serde(default)]
   pub debug: Option<String>,
   #[serde(default)]
   pub tree:  Option<String>,
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
   pub remove:   Vec<String>,
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
   #[serde(default)]
   pub sys:      Option<Vec<String>>,
   #[serde(flatten)]
   pub systems:  Map<String, Value>,
}

#[derive(Deserialize, Serialize, Clone)]
#[serde(untagged)]
pub enum Def {
   Flag(bool),
   Text(String),
   Number(i64),
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Platform {
   pub binfmt:       BinaryFormat,
   #[serde(default)]
   pub hardening:    BTreeMap<Hardening, bool>,
   #[serde(default)]
   pub cross:        bool,
   #[serde(default)]
   pub cpu:          String,
   #[serde(default)]
   pub os:           String,
   pub posix:        bool,
   pub ext:          Extensions,
   pub bits:         u8,
   pub endian:       String,
   #[serde(default)]
   pub build_triple: String,
   #[serde(default)]
   pub launch:       String,
   #[serde(default)]
   pub interp:       String,
   #[serde(default)]
   pub reloc_stub:   String,
   #[serde(default)]
   pub sysroot:      String,
   #[serde(default)]
   pub gnu_triple:   String,
   #[serde(default)]
   pub rust_triple:  String,
   #[serde(default)]
   pub emulator:     Vec<String>,
   #[serde(default)]
   pub os_names:     OsNames,
   #[serde(default)]
   pub names:        Names,
}

#[derive(Deserialize)]
pub struct Extensions {
   pub exe:    String,
   pub shared: String,
   pub import: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct OsNames {
   #[serde(default)]
   pub cmake:        String,
   #[serde(default)]
   pub go:           String,
   #[serde(default)]
   pub uname:        String,
   #[serde(default)]
   pub meson:        String,
   #[serde(default)]
   pub meson_kernel: String,
}

#[derive(Deserialize, Default)]
pub struct Names {
   #[serde(default)]
   pub go:    String,
   #[serde(default)]
   pub meson: String,
}

record! {
   #[derive(Default)]
   pub Tests {
      optional {
         run ("run"): bool,
         separate ("separate"): bool,
         parallel ("parallel"): bool,
         version ("version"): VersionCheck,
         dlopen ("dlopen"): Vec<String>,
         dependencies ("dependencies"): Vec<PackagePath>,
      }
   }
}

#[derive(Clone, Deserialize, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum VersionCheck {
   Enabled(bool),
   Command(String),
}

impl OptionType for VersionCheck {
   fn kind() -> Type {
      Type::union(vec![Type::Boolean, Type::String])
   }
}

/// How much DWARF a compile keeps when the package has a debug output.
#[derive(Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(try_from = "String")]
pub enum DebugInfo {
   Full,
   Lines,
}

impl DebugInfo {
   const ALL: [Self; 2] = [Self::Full, Self::Lines];

   const fn name(self) -> &'static str {
      match self {
         Self::Full => "full",
         Self::Lines => "lines",
      }
   }
}

impl TryFrom<String> for DebugInfo {
   type Error = io::Error;

   fn try_from(value: String) -> io::Result<Self> {
      Self::ALL
         .into_iter()
         .find(|level| level.name() == value)
         .ok_or_else(|| io::Error::other(format!("unknown debugInfo {value}")))
   }
}

impl Serialize for DebugInfo {
   fn serialize<Encoder>(&self, serializer: Encoder) -> Result<Encoder::Ok, Encoder::Error>
   where
      Encoder: Serializer,
   {
      serializer.serialize_str(self.name())
   }
}

impl OptionType for DebugInfo {
   fn kind() -> Type {
      Type::Enum {
         values: Self::ALL.map(|level| level.name().to_owned()).to_vec(),
      }
   }
}

record! {
   pub Cc {
      optional {
         hardening ("hardening"): Choice,
         debug_info ("debugInfo"): DebugInfo,
         cflags ("cflags"): Vec<String>,
         cxxflags ("cxxflags"): Vec<String>,
         ldflags ("ldflags"): Vec<String>,
      }
   }
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

   pub fn options<Options>(&self, system: &str) -> io::Result<Options>
   where
      Options: DeserializeOwned,
   {
      let mut found = self
         .spec
         .get(system)
         .cloned()
         .ok_or_else(|| io::Error::other(format!("spec has no {system} options")))?;

      if let Some(options) = found.as_object_mut() {
         options.remove("tool");
      }

      Ok(serde_json::from_value(found)?)
   }

   pub fn project_dir(&self, root: &str) -> PathBuf {
      normalize(&Path::new(&self.src).join(root))
   }

   pub fn spec(&self) -> io::Result<Spec> {
      Ok(serde_json::from_value(self.spec.clone())?)
   }

   pub fn platform(&self) -> io::Result<Platform> {
      Ok(serde_json::from_value(self.platform.clone())?)
   }

   pub fn test_jobs(&self) -> io::Result<usize> {
      Ok(if self.spec()?.tests.parallel.unwrap_or(true) {
         self.njobs
      } else {
         1
      })
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
