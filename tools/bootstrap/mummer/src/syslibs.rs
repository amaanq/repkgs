//! The lock side of the `[bindings]` tables in act.toml.
//!
//! A package names the locked third-party packages that link it. spieler
//! decides act.lock's `sys` from the lock files of a tree with them, bigtop
//! gets the tables of a build's dependencies and turns their settings on.

use std::{
   collections::{
      BTreeMap,
      BTreeSet,
      HashMap,
   },
   fs::{
      canonicalize,
      read_dir,
      read_to_string,
   },
   io,
   path::{
      Path,
      PathBuf,
   },
   process::Command,
};

use pound::{
   ArgValue as _,
   ValueEnum,
};
use serde::{
   Deserialize,
   Serialize,
};

use crate::{
   files::utf8,
   process::loose,
};

/// The locked packages of one ecosystem that link a package, written as a
/// bare list when they need nothing beyond the names. A name with `flags`
/// or `configSettings` is one of them without being listed.
#[derive(Default, Deserialize)]
#[serde(from = "Spelled")]
pub struct Binding {
   pub names:           BTreeSet<String>,
   pub env:             BTreeMap<String, String>,
   pub tags:            Vec<String>,
   pub flags:           BTreeMap<String, Vec<String>>,
   pub config_settings: BTreeMap<String, BTreeMap<String, String>>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Spelled {
   Names(BTreeSet<String>),
   Table {
      #[serde(default)]
      names:           BTreeSet<String>,
      #[serde(default)]
      env:             BTreeMap<String, String>,
      #[serde(default)]
      tags:            Vec<String>,
      #[serde(default)]
      flags:           BTreeMap<String, Vec<String>>,
      #[serde(default, rename = "configSettings")]
      config_settings: BTreeMap<String, BTreeMap<String, String>>,
   },
}

impl From<Spelled> for Binding {
   #[inline]
   fn from(spelled: Spelled) -> Self {
      match spelled {
         Spelled::Names(names) => {
            Self {
               names,
               ..Self::default()
            }
         },
         Spelled::Table {
            mut names,
            env,
            tags,
            flags,
            config_settings,
         } => {
            names.extend(flags.keys().cloned());
            names.extend(config_settings.keys().cloned());
            Self {
               names,
               env,
               tags,
               flags,
               config_settings,
            }
         },
      }
   }
}

/// The package ecosystems a binding can name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Deserialize, ValueEnum)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum Ecosystem {
   Cargo,
   Go,
   Python,
   Gems,
   Hackage,
}

impl Ecosystem {
   /// The lock file a source of this ecosystem carries, none for Hackage,
   /// whose set is solved apart from the source.
   #[inline]
   #[must_use]
   pub const fn lock_file(self) -> Option<&'static str> {
      match self {
         Self::Cargo => Some("Cargo.lock"),
         Self::Go => Some("go.sum"),
         Self::Python => Some("uv.lock"),
         Self::Gems => Some("Gemfile.lock"),
         Self::Hackage => None,
      }
   }
}

pub type Bindings = BTreeMap<Ecosystem, Binding>;

/// Packages of the set by name, each with its bindings.
pub type Tables = BTreeMap<String, Bindings>;

/// Tables as JSON, the way Nix hands them to a build.
#[inline]
pub fn load(path: &Path) -> io::Result<Tables> {
   Ok(serde_json::from_str(&read_to_string(path)?)?)
}

#[derive(Deserialize)]
struct Act {
   #[serde(default)]
   bindings: Bindings,
}

/// The tables of every `pkgs/<shard>/<name>/act.toml` under `pkgs`.
#[inline]
pub fn collect(pkgs: &Path) -> io::Result<Tables> {
   let mut tables = Tables::new();
   for entry in read_dir(pkgs)? {
      let shard = entry?.path();
      if !shard.is_dir() {
         continue;
      }
      for package in read_dir(&shard)? {
         let dir = package?.path();
         let file = dir.join("act.toml");
         if !file.is_file() {
            continue;
         }
         let act = toml::from_str::<Act>(&read_to_string(&file)?)
            .map_err(|error| io::Error::other(format!("{}: {error}", file.display())))?;
         if !act.bindings.is_empty() {
            let name = utf8(Path::new(dir.file_name().unwrap_or_default()))?;
            tables.insert(name.to_owned(), act.bindings);
         }
      }
   }
   Ok(tables)
}

/// Package names in one ecosystem's lock file.
#[inline]
#[must_use]
pub fn locked(ecosystem: Ecosystem, text: &str) -> Vec<String> {
   match ecosystem {
      Ecosystem::Cargo | Ecosystem::Python => {
         text
            .lines()
            .filter_map(|line| line.strip_prefix("name = \""))
            .filter_map(|rest| rest.strip_suffix('"'))
            .map(str::to_owned)
            .collect()
      },
      Ecosystem::Go => {
         text
            .lines()
            .filter_map(|line| line.split(' ').next())
            .filter(|path| !path.is_empty())
            .map(str::to_owned)
            .collect()
      },
      Ecosystem::Gems => {
         text
            .lines()
            .filter_map(|line| line.strip_prefix("    "))
            .filter(|line| !line.starts_with(' '))
            .filter_map(|line| line.split_once(" ("))
            .map(|(name, _)| name.to_owned())
            .collect()
      },
      Ecosystem::Hackage => Vec::new(),
   }
}

/// Our packages the locked names of one ecosystem can link, sorted.
#[inline]
#[must_use]
pub fn wanted_for(tables: &Tables, ecosystem: Ecosystem, names: &[String]) -> Vec<String> {
   tables
      .iter()
      .filter(|&(_, bindings)| {
         bindings
            .get(&ecosystem)
            .is_some_and(|binding| names.iter().any(|name| binding.names.contains(name)))
      })
      .map(|(pkg, _)| pkg.clone())
      .collect()
}

/// Our packages the lock files in `dir` can link, sorted, what act.lock's `sys`
/// should say. A Cargo.lock counts only with `cargo`, the build it is resolved
/// for.
#[inline]
pub fn wanted(tables: &Tables, dir: &Path, cargo: Option<&CargoBuild>) -> io::Result<Vec<String>> {
   let mut found = BTreeSet::new();
   for ecosystem in Ecosystem::ALL.iter().copied() {
      let Some(file) = ecosystem.lock_file() else {
         continue;
      };
      let Ok(text) = read_to_string(dir.join(file)) else {
         continue;
      };
      let names = match (ecosystem, cargo) {
         (Ecosystem::Cargo, Some(build)) => shipped_crates(dir, build)?,
         (Ecosystem::Cargo, None) => continue,
         _ => locked(ecosystem, &text),
      };
      found.extend(wanted_for(tables, ecosystem, &names));
   }
   Ok(found.into_iter().collect())
}

/// What a cargo build turns on, from its recipe's `[cargo]` options, and the
/// `CARGO_HOME` whose config already points at vendored crates, if any.
#[derive(Clone, Default, Deserialize, Serialize)]
pub struct CargoBuild {
   #[serde(default)]
   pub features:   Vec<String>,
   #[serde(default, rename = "noDefaultFeatures")]
   pub no_default: bool,
   #[serde(default, rename = "withoutFeatures")]
   pub without:    Vec<String>,
   #[serde(skip)]
   pub home:       Option<PathBuf>,
}

#[derive(Deserialize)]
struct Metadata {
   packages:          Vec<MetadataPackage>,
   workspace_members: Vec<String>,
   resolve:           Option<Resolve>,
}

#[derive(Deserialize)]
struct MetadataPackage {
   id:            String,
   name:          String,
   manifest_path: PathBuf,
   features:      BTreeMap<String, Vec<String>>,
}

#[derive(Deserialize)]
struct Resolve {
   nodes: Vec<Node>,
}

#[derive(Deserialize)]
struct Node {
   id:   String,
   deps: Vec<NodeDep>,
}

#[derive(Deserialize)]
struct NodeDep {
   pkg:       String,
   dep_kinds: Vec<DepKind>,
}

#[derive(Deserialize)]
struct DepKind {
   kind: Option<Dependency>,
}

/// A dependency kind cargo metadata names, a normal one being none.
#[derive(Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Dependency {
   Dev,
   Build,
}

fn metadata(dir: &Path, flags: &[String], build: &CargoBuild) -> io::Result<Metadata> {
   let mut command = Command::new("cargo");
   command
      .args(["metadata", "--format-version", "1", "--locked"])
      .args(flags)
      .current_dir(dir);
   if let Some(ref home) = build.home {
      command.env("CARGO_HOME", home);
   }
   let output = command.output()?;
   if !output.status.success() {
      return Err(io::Error::other(format!(
         "cargo metadata in {} failed: {}",
         dir.display(),
         loose(&output.stderr).trim()
      )));
   }
   Ok(serde_json::from_slice(&output.stdout)?)
}

/// cargo's flags for `build`, `without` taken out of the package's own
/// default set as cargo reads it.
///
/// # Errors
///
/// When cargo cannot read the package, or `without` names a feature that is
/// not a default one.
#[inline]
pub fn feature_flags(dir: &Path, build: &CargoBuild) -> io::Result<Vec<String>> {
   let mut flags = Vec::new();
   let mut features = build.features.clone();
   if build.no_default || !build.without.is_empty() {
      flags.push("--no-default-features".to_owned());
   }
   if !build.without.is_empty() {
      let manifest = canonicalize(dir.join("Cargo.toml"))?;
      let found = metadata(dir, &["--no-deps".to_owned()], build)?;
      let package = found
         .packages
         .into_iter()
         .find(|package| package.manifest_path == manifest)
         .ok_or_else(|| {
            io::Error::other(format!("cargo metadata lists no {}", manifest.display()))
         })?;
      let defaults = package.features.get("default").cloned().unwrap_or_default();
      if let Some(missing) = build.without.iter().find(|name| !defaults.contains(name)) {
         return Err(io::Error::other(format!(
            "withoutFeatures names {missing}, which is not among the default features {}",
            defaults.join(", ")
         )));
      }
      features.extend(
         defaults
            .into_iter()
            .filter(|name| !build.without.contains(name)),
      );
   }
   if !features.is_empty() {
      flags.push(format!("--features={}", features.join(",")));
   }
   Ok(flags)
}

/// The crates the build compiles, from cargo's resolve with its features:
/// what the workspace members need, their dev-dependencies left out.
fn shipped_crates(dir: &Path, build: &CargoBuild) -> io::Result<Vec<String>> {
   let found = metadata(dir, &feature_flags(dir, build)?, build)?;
   let names = found
      .packages
      .iter()
      .map(|package| (package.id.as_str(), package.name.as_str()))
      .collect::<HashMap<_, _>>();
   let nodes = found
      .resolve
      .as_ref()
      .map(|resolve| {
         resolve
            .nodes
            .iter()
            .map(|node| (node.id.as_str(), node))
            .collect::<HashMap<_, _>>()
      })
      .unwrap_or_default();
   let mut reached = BTreeSet::new();
   let mut pending = found
      .workspace_members
      .iter()
      .map(String::as_str)
      .collect::<Vec<_>>();

   while let Some(id) = pending.pop() {
      if !reached.insert(id) {
         continue;
      }
      let member = found.workspace_members.iter().any(|member| member == id);
      for dependency in nodes
         .get(id)
         .map(|node| node.deps.as_slice())
         .unwrap_or_default()
      {
         let dev_only = dependency
            .dep_kinds
            .iter()
            .all(|kind| kind.kind == Some(Dependency::Dev));
         if !(member && dev_only) {
            pending.push(&dependency.pkg);
         }
      }
   }

   Ok(reached
      .into_iter()
      .filter_map(|id| names.get(id).map(|&name| name.to_owned()))
      .collect())
}
