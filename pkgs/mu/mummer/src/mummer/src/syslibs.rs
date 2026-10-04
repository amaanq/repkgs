//! The lock side of builder/sys-libs.json.
//!
//! Locked third-party packages that link a C library and which package of
//! ours provides it. spieler decides act.lock's `sys` from the lock files of a
//! tree with it, bigtop checks the pin at build time.

use std::{
   collections::{
      BTreeMap,
      BTreeSet,
   },
   fs::{
      read_dir,
      read_to_string,
   },
   io,
   path::Path,
};

use serde::Deserialize;
use toml::Table;

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

pub type Tables = BTreeMap<String, BTreeMap<String, Entry>>;

pub const LOCKS: [(&str, &str); 4] = [
   ("cargo", "Cargo.lock"),
   ("go", "go.sum"),
   ("python", "uv.lock"),
   ("gems", "Gemfile.lock"),
];

#[inline]
pub fn load(path: &Path) -> io::Result<Tables> {
   Ok(serde_json::from_str(&read_to_string(path)?)?)
}

/// Package names in one ecosystem's lock file.
#[must_use]
#[inline]
pub fn locked(ecosystem: &str, text: &str) -> Vec<String> {
   match ecosystem {
      "cargo" | "python" => {
         text
            .lines()
            .filter_map(|line| line.strip_prefix("name = \""))
            .filter_map(|rest| rest.strip_suffix('"'))
            .map(str::to_owned)
            .collect()
      },
      "go" => {
         text
            .lines()
            .filter_map(|line| line.split(' ').next())
            .filter(|path| !path.is_empty())
            .map(str::to_owned)
            .collect()
      },
      "gems" => {
         text
            .lines()
            .filter_map(|line| line.strip_prefix("    "))
            .filter(|line| !line.starts_with(' '))
            .filter_map(|line| line.split_once(" ("))
            .map(|(name, _)| name.to_owned())
            .collect()
      },
      _ => Vec::new(),
   }
}

/// Our packages the locked names of one ecosystem can link, sorted.
#[must_use]
#[inline]
pub fn wanted_for(tables: &Tables, ecosystem: &str, names: &[String]) -> Vec<String> {
   let mut found = Vec::new();
   if let Some(table) = tables.get(ecosystem) {
      for (name, entry) in table {
         if let Some(ref package) = entry.pkg
            && names.contains(name)
            && !found.contains(package)
         {
            found.push(package.clone());
         }
      }
   }
   found.sort();
   found
}

/// Our packages the lock files in `dir` can link, sorted, what act.lock's `sys`
/// should say.
#[inline]
pub fn wanted(tables: &Tables, dir: &Path) -> io::Result<Vec<String>> {
   let mut found = Vec::new();
   for (ecosystem, file) in LOCKS {
      let Ok(text) = read_to_string(dir.join(file)) else {
         continue;
      };
      let names = if ecosystem == "cargo" {
         shipped_crates(dir, &text)?
      } else {
         locked(ecosystem, &text)
      };
      for pkg in wanted_for(tables, ecosystem, &names) {
         if !found.contains(&pkg) {
            found.push(pkg);
         }
      }
   }
   found.sort();
   Ok(found)
}

#[derive(Deserialize)]
struct CargoLock {
   #[serde(default)]
   package: Vec<LockedCrate>,
}

#[derive(Deserialize)]
struct LockedCrate {
   name:         String,
   version:      String,
   source:       Option<String>,
   #[serde(default)]
   dependencies: Vec<String>,
}

/// Cargo.lock names minus the crates only a workspace member's
/// `[dev-dependencies]` reach, which never link into what gets installed.
fn shipped_crates(dir: &Path, text: &str) -> io::Result<Vec<String>> {
   let lock = toml::from_str::<CargoLock>(text).map_err(io::Error::other)?;
   let manifests = member_manifests(dir, 3)?;
   let mut reached = BTreeSet::new();
   let mut pending = Vec::new();

   for member in lock.package.iter().filter(|krate| krate.source.is_none()) {
      let dev_only = manifests
         .get(&member.name)
         .map(dev_only_names)
         .unwrap_or_default();
      reached.insert((member.name.as_str(), member.version.as_str()));
      for dependency in &member.dependencies {
         if !dev_only.contains(dependency.split(' ').next().unwrap_or_default()) {
            pending.push(dependency.as_str());
         }
      }
   }

   while let Some(dependency) = pending.pop() {
      let mut parts = dependency.split(' ');
      let name = parts.next().unwrap_or_default();
      let version = parts.next();
      for krate in lock
         .package
         .iter()
         .filter(|krate| krate.name == name && version.is_none_or(|wanted| krate.version == wanted))
      {
         if reached.insert((krate.name.as_str(), krate.version.as_str())) {
            pending.extend(krate.dependencies.iter().map(String::as_str));
         }
      }
   }

   Ok(reached
      .into_iter()
      .map(|(name, _)| name.to_owned())
      .collect())
}

/// Package name to manifest for every Cargo.toml within `depth` levels of
/// `dir`.
fn member_manifests(dir: &Path, depth: u32) -> io::Result<BTreeMap<String, Table>> {
   let mut found = BTreeMap::new();
   if let Ok(text) = read_to_string(dir.join("Cargo.toml")) {
      let manifest = toml::from_str::<Table>(&text).map_err(io::Error::other)?;
      if let Some(name) = manifest
         .get("package")
         .and_then(|package| package.get("name"))
         .and_then(toml::Value::as_str)
      {
         found.insert(name.to_owned(), manifest);
      }
   }
   if depth == 0 {
      return Ok(found);
   }
   for entry in read_dir(dir)? {
      let path = entry?.path();
      let skipped = path
         .file_name()
         .and_then(|name| name.to_str())
         .is_none_or(|name| name.starts_with('.') || matches!(name, "target" | "vendor"));
      if path.is_dir() && !skipped {
         found.extend(member_manifests(&path, depth - 1)?);
      }
   }
   Ok(found)
}

/// Crates a manifest names under `[dev-dependencies]` and nowhere else.
fn dev_only_names(manifest: &Table) -> BTreeSet<String> {
   let names = |kind: &str| {
      let mut tables = vec![manifest.get(kind)];
      if let Some(targets) = manifest.get("target").and_then(toml::Value::as_table) {
         tables.extend(targets.values().map(|target| target.get(kind)));
      }
      tables
         .into_iter()
         .flatten()
         .filter_map(toml::Value::as_table)
         .flat_map(|table| {
            table.iter().map(|(key, value)| {
               value
                  .get("package")
                  .and_then(toml::Value::as_str)
                  .unwrap_or(key)
                  .to_owned()
            })
         })
         .collect::<BTreeSet<_>>()
   };
   let linked = names("dependencies")
      .union(&names("build-dependencies"))
      .cloned()
      .collect::<BTreeSet<_>>();
   names("dev-dependencies")
      .into_iter()
      .filter(|name| !linked.contains(name))
      .collect()
}
