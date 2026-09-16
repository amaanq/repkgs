//! The lock side of builder/sys-libs.json.
//!
//! Locked third-party packages that link a C library and which package of
//! ours provides it. uptrack decides `[pin] sys` from the lock files of a
//! tree with it, jib checks the pin at build time.

use std::{
   collections::BTreeMap,
   fs::read_to_string,
   io,
   path::Path,
};

use serde::Deserialize;

#[derive(Deserialize)]
pub struct Entry {
   pub pkg:   String,
   #[serde(default)]
   pub env:   BTreeMap<String, String>,
   #[serde(default)]
   pub tags:  Vec<String>,
   #[serde(default)]
   pub flags: Vec<String>,
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
         if names.contains(name) && !found.contains(&entry.pkg) {
            found.push(entry.pkg.clone());
         }
      }
   }
   found.sort();
   found
}

/// Our packages the lock files in `dir` can link, sorted, what `[pin] sys`
/// should say.
#[inline]
pub fn wanted(tables: &Tables, dir: &Path) -> io::Result<Vec<String>> {
   let mut found = Vec::new();
   for (ecosystem, file) in LOCKS {
      let Ok(text) = read_to_string(dir.join(file)) else {
         continue;
      };
      for pkg in wanted_for(tables, ecosystem, &locked(ecosystem, &text)) {
         if !found.contains(&pkg) {
            found.push(pkg);
         }
      }
   }
   found.sort();
   Ok(found)
}
