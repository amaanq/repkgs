//! The lock side of builder/sys-libs.json.
//!
//! Locked third-party packages that link a C library and which package of
//! ours provides it. spieler decides act.lock's `sys` from the lock files of a
//! tree with it, bigtop checks the pin at build time.

use std::{
   collections::{
      BTreeMap,
      BTreeSet,
      HashMap,
   },
   fmt::{
      Display,
      Formatter,
      Result as FmtResult,
   },
   fs::{
      canonicalize,
      read_to_string,
   },
   io,
   path::{
      Path,
      PathBuf,
   },
   process::Command,
   str::FromStr,
};

use serde::{
   Deserialize,
   Serialize,
};

#[derive(Deserialize)]
pub struct Entry {
   pub pkg:             Option<String>,
   #[serde(default)]
   pub env:             BTreeMap<String, String>,
   #[serde(default)]
   pub tags:            Vec<String>,
   #[serde(default)]
   pub flags:           Vec<String>,
   #[serde(default, rename = "configSettings")]
   pub config_settings: BTreeMap<String, String>,
}

/// The package ecosystems sys-libs.json has a table for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
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
   #[must_use]
   #[inline]
   pub const fn lock_file(self) -> Option<&'static str> {
      match self {
         Self::Cargo => Some("Cargo.lock"),
         Self::Go => Some("go.sum"),
         Self::Python => Some("uv.lock"),
         Self::Gems => Some("Gemfile.lock"),
         Self::Hackage => None,
      }
   }

   const ALL: [Self; 5] = [
      Self::Cargo,
      Self::Go,
      Self::Python,
      Self::Gems,
      Self::Hackage,
   ];
}

impl Display for Ecosystem {
   #[inline]
   #[expect(
      clippy::renamed_function_params,
      reason = "formatter describes the argument"
   )]
   fn fmt(&self, formatter: &mut Formatter<'_>) -> FmtResult {
      formatter.write_str(match *self {
         Self::Cargo => "cargo",
         Self::Go => "go",
         Self::Python => "python",
         Self::Gems => "gems",
         Self::Hackage => "hackage",
      })
   }
}

impl FromStr for Ecosystem {
   type Err = io::Error;

   #[inline]
   fn from_str(text: &str) -> io::Result<Self> {
      Self::ALL
         .into_iter()
         .find(|ecosystem| ecosystem.to_string() == text)
         .ok_or_else(|| io::Error::other(format!("no ecosystem {text}")))
   }
}

pub type Tables = BTreeMap<Ecosystem, BTreeMap<String, Entry>>;

#[inline]
pub fn load(path: &Path) -> io::Result<Tables> {
   Ok(serde_json::from_str(&read_to_string(path)?)?)
}

/// Package names in one ecosystem's lock file.
#[must_use]
#[inline]
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
#[must_use]
#[inline]
pub fn wanted_for(tables: &Tables, ecosystem: Ecosystem, names: &[String]) -> Vec<String> {
   tables
      .get(&ecosystem)
      .into_iter()
      .flatten()
      .filter(|&(name, _)| names.contains(name))
      .filter_map(|(_, entry)| entry.pkg.clone())
      .collect::<BTreeSet<_>>()
      .into_iter()
      .collect()
}

/// Our packages the lock files in `dir` can link, sorted, what act.lock's `sys`
/// should say. A Cargo.lock counts only with `cargo`, the build it is resolved
/// for.
#[inline]
pub fn wanted(tables: &Tables, dir: &Path, cargo: Option<&CargoBuild>) -> io::Result<Vec<String>> {
   let mut found = BTreeSet::new();
   for ecosystem in Ecosystem::ALL {
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
         String::from_utf8_lossy(&output.stderr).trim()
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
