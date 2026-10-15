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

use mummer::{
   attrs::load,
   config::BinaryFormat,
   store::{
      STORE_DIR,
      is_store_path,
      normalize,
   },
   syslibs::Tables,
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
   exports::{
      Exported,
      Exports,
   },
   hardening::{
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
   /// On PATH in test phases only.
   #[serde(default, rename = "testDependencies")]
   pub test_dependencies:  Vec<String>,
   #[serde(default)]
   pub plumbing:           Vec<String>,
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
   #[serde(rename = "sysLibs", default)]
   pub sys_libs:           Tables,
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
   pub role: Role,
   #[serde(flatten)]
   pub kind: Kind,
}

#[derive(Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Role {
   Build,
   Setup,
   Test,
}

#[derive(Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Kind {
   System { system: String, verb: String },
   Nu { body: String },
}

#[derive(Deserialize)]
pub struct Outputs {
   pub out:    String,
   #[serde(default)]
   pub debug:  Option<String>,
   #[serde(default)]
   pub tree:   Option<String>,
   #[serde(default)]
   pub dev:    Option<String>,
   /// The outputs a recipe's `outputs` names, by name.
   #[serde(flatten)]
   pub carved: BTreeMap<String, String>,
}

impl Attrs {
   pub fn load() -> io::Result<Self> {
      load()
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
   pub prebuilt: Prebuilt,
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
   pub exports:  Exported,
   #[serde(default)]
   pub cc:       Option<Cc>,
   #[serde(default)]
   pub env:      BTreeMap<String, Value>,
   #[serde(default)]
   pub uses:     Vec<String>,
   #[serde(default)]
   pub sys:      Option<Vec<String>>,
   #[serde(default)]
   pub outputs:  BTreeMap<String, Carved>,
   #[serde(flatten)]
   pub systems:  Map<String, Value>,
}

record! {
   pub Carved {
      required {
         files ("files"): Vec<String>,
      }
      optional {
         linked ("linked"): bool,
      }
   }
}

#[derive(Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(try_from = "PrebuiltSpelling")]
pub enum Prebuilt {
   #[default]
   Source,
   Implanted,
   Ldso,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum PrebuiltSpelling {
   Unset(()),
   Flag(bool),
   Mode(String),
}

impl TryFrom<PrebuiltSpelling> for Prebuilt {
   type Error = io::Error;

   fn try_from(spelling: PrebuiltSpelling) -> io::Result<Self> {
      match spelling {
         PrebuiltSpelling::Unset(()) | PrebuiltSpelling::Flag(false) => Ok(Self::Source),
         PrebuiltSpelling::Flag(true) => Ok(Self::Implanted),
         PrebuiltSpelling::Mode(mode) if mode == "ldso" => Ok(Self::Ldso),
         PrebuiltSpelling::Mode(mode) => Err(io::Error::other(format!("unknown prebuilt {mode}"))),
      }
   }
}

#[derive(Deserialize, Serialize, Clone)]
#[serde(untagged)]
pub enum Def {
   Flag(bool),
   Text(String),
   Number(i64),
   List(Vec<String>),
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Platform {
   #[serde(default)]
   pub name:              String,
   pub binfmt:            BinaryFormat,
   #[serde(default)]
   pub hardening:         BTreeMap<Hardening, bool>,
   #[serde(default)]
   pub cross:             bool,
   #[serde(default)]
   pub transparent:       bool,
   #[serde(default)]
   pub probe:             String,
   #[serde(default)]
   pub cpu:               String,
   #[serde(default)]
   pub os:                String,
   pub posix:             bool,
   pub ext:               Extensions,
   pub bits:              u8,
   pub endian:            String,
   #[serde(default)]
   pub build_triple:      String,
   #[serde(default)]
   pub build_rust_triple: String,
   #[serde(default)]
   pub dlaudit:           String,
   #[serde(default)]
   pub launch:            String,
   #[serde(default)]
   pub interp:            String,
   #[serde(default)]
   pub reloc_stub:        String,
   #[serde(default)]
   pub sysroot:           String,
   #[serde(default)]
   pub gnu_triple:        String,
   #[serde(default)]
   pub rust_triple:       String,
   #[serde(default)]
   pub emulator:          Vec<String>,
   #[serde(default)]
   pub os_names:          OsNames,
   #[serde(default)]
   pub names:             Names,
}

#[derive(Clone, Deserialize)]
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
         etc ("etc"): bool,
         fhs ("fhs"): bool,
         terminal ("terminal"): bool,
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

/// How much DWARF a compile keeps when the package has a debug output, line
/// tables unless a package asks for full.
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

/// The linker a package's links run. The set picks wild on ELF except for
/// wild's own build closure, lld elsewhere.
#[derive(Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(try_from = "String")]
pub enum Linker {
   Lld,
   Wild,
}

impl Linker {
   const ALL: [Self; 2] = [Self::Lld, Self::Wild];

   pub const fn name(self) -> &'static str {
      match self {
         Self::Lld => "lld",
         Self::Wild => "wild",
      }
   }
}

impl TryFrom<String> for Linker {
   type Error = io::Error;

   fn try_from(value: String) -> io::Result<Self> {
      Self::ALL
         .into_iter()
         .find(|linker| linker.name() == value)
         .ok_or_else(|| io::Error::other(format!("unknown linker {value}")))
   }
}

impl Serialize for Linker {
   fn serialize<Encoder>(&self, serializer: Encoder) -> Result<Encoder::Ok, Encoder::Error>
   where
      Encoder: Serializer,
   {
      serializer.serialize_str(self.name())
   }
}

impl OptionType for Linker {
   fn kind() -> Type {
      Type::Enum {
         values: Self::ALL.map(|linker| linker.name().to_owned()).to_vec(),
      }
   }
}

record! {
   pub Cc {
      optional {
         hardening ("hardening"): Choice,
         debug_info ("debugInfo"): DebugInfo,
         linker ("linker"): Linker,
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
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Ctx {
   pub spec:      Value,
   pub out:       PathBuf,
   pub dest:      PathBuf,
   #[serde(default, skip_serializing_if = "Option::is_none")]
   pub dev:       Option<PathBuf>,
   pub deps:      Vec<Exports>,
   pub roots:     Vec<String>,
   pub njobs:     usize,
   pub src:       PathBuf,
   pub build:     PathBuf,
   pub platform:  Value,
   pub tests_run: bool,
   pub cache:     bool,
   /// What test phases add to the build environment, their test dependencies.
   #[serde(default)]
   pub test_env:  BTreeMap<String, TestVar>,
}

/// A variable test phases change. A list such as PATH gets the test
/// dependencies' entries in front, so their tools win over the build's.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TestVar {
   Prepend {
      separator: String,
      entries:   Vec<String>,
   },
   Set(String),
}

impl TestVar {
   /// How `test` differs from `base`, the build environment's value.
   #[must_use = "the change is the whole answer"]
   pub fn between(base: Option<&Value>, tested: &Value) -> Option<Self> {
      let flat = |value: &Value| {
         match *value {
            Value::Array(ref items) => {
               items
                  .iter()
                  .filter_map(Value::as_str)
                  .collect::<Vec<_>>()
                  .join(":")
            },
            Value::String(ref plain) => plain.clone(),
            Value::Bool(flag) => flag.to_string(),
            Value::Number(ref number) => number.to_string(),
            Value::Null | Value::Object(_) => String::new(),
         }
      };
      let wanted = flat(tested);
      let had = base.map(flat).unwrap_or_default();
      if wanted == had {
         return None;
      }
      let separator = if wanted.contains(' ') { " " } else { ":" };
      let old = had.split(separator).collect::<Vec<_>>();
      let new = wanted.split(separator).collect::<Vec<_>>();
      let listed = !had.is_empty() && !wanted.contains(['{', '"']);
      if !listed || !old.iter().all(|entry| new.contains(entry)) {
         return Some(Self::Set(wanted));
      }
      Some(Self::Prepend {
         separator: separator.to_owned(),
         entries:   new
            .into_iter()
            .filter(|entry| !old.contains(entry))
            .map(str::to_owned)
            .collect(),
      })
   }
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
      normalize(&self.src.join(root))
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

pub fn nix_store() -> String {
   var("NIX_STORE").unwrap_or_else(|_unset| STORE_DIR.to_owned())
}

pub fn build_top() -> io::Result<String> {
   var("NIX_BUILD_TOP").map_err(|_unset| io::Error::other("NIX_BUILD_TOP is not set"))
}
