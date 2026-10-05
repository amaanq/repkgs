//! `locks/hackage.toml` retains each Haskell package release separately.
//! `<pkg>-<version> = { src, rev, cabal }`.
//!
//! `src` the sha256 of hackage's `<pkg>-<version>.tar.gz` (hackage publishes
//! none, prefetched), `rev`/`cabal` the newest .cabal revision and its
//! sha256 (hackage's JSON). fetch.hackageSet lays the whole table out as one
//! file+noindex repository every cabal package solves against offline.
//!
//! Locking a package = `cabal freeze` of its source with the tree's
//! ghc-bootstrap/cabal-bootstrap, the current set as solver preferences so
//! packages agree on versions where they can. Different releases coexist
//! because Hadrian and cabal-install need different Cabal versions. GHC's
//! boot packages appear in freeze output too and 404 on hackage: skipped.

use std::{
   collections::BTreeMap,
   env::{
      temp_dir,
      var,
   },
   ffi::OsStr,
   fs::{
      read_dir,
      read_to_string,
      remove_dir_all,
      write,
   },
   io,
   path::{
      Path,
      PathBuf,
   },
   process::{
      Command,
      id,
   },
   time::Duration,
};

use mummer::{
   hackage::PackageId,
   tree::parallel,
};
use regex_lite::Regex;
use serde_json::Value;

use crate::{
   http,
   locks::Entries,
   nix,
   pipeline::root,
};

const HACKAGE: &str = "https://hackage.haskell.org/package";

fn tool(attr: &str, bin: &str) -> io::Result<PathBuf> {
   Ok(PathBuf::from(nix::build(&root(), attr)?)
      .join("bin")
      .join(bin))
}

fn cabal_home() -> PathBuf {
   var("XDG_CACHE_HOME")
      .map_or_else(
         |_unset| PathBuf::from(var("HOME").unwrap_or_default()).join(".cache"),
         PathBuf::from,
      )
      .join("spieler")
      .join("cabal")
}

fn has_index(home: &Path) -> bool {
   read_dir(home.join("packages")).is_ok_and(|entries| {
      entries
         .flatten()
         .any(|entry| entry.path().join("01-index.tar").exists())
   })
}

/// What cabal's solver picks for the project at `src`/`sub` with the
/// package's cabal options, preferring `old`. The whole source is copied,
/// cabal.project may name siblings.
fn solve(src: &Path, sub: &str, options: &Value, old: &Entries) -> io::Result<Vec<PackageId>> {
   let ghc = tool("ghc-bootstrap", "ghc")?;
   let cabal = tool("cabal-bootstrap", "cabal")?;
   let home = cabal_home();
   let top = temp_dir().join(format!("spieler-cabal.{}", id()));
   drop(remove_dir_all(&top));
   nix::succeed("cp", [
      OsStr::new("-r"),
      OsStr::new("--no-preserve=mode"),
      src.join(".").as_os_str(),
      top.as_os_str(),
   ])?;
   let work = top.join(sub);
   let project = work.join("cabal.project");
   let mut versions: BTreeMap<String, Vec<String>> = BTreeMap::new();
   for key in old.keys() {
      let package: PackageId = key.parse()?;
      versions
         .entry(package.name().to_owned())
         .or_default()
         .push(format!("=={}", package.version()));
   }

   let preferences: Vec<String> = versions
      .into_iter()
      .map(|(name, releases)| format!("{name} {}", releases.join(" || ")))
      .collect();
   let base = read_to_string(&project).unwrap_or_else(|_missing| "packages: .".to_owned());
   let extra = options
      .get("project")
      .and_then(Value::as_str)
      .unwrap_or_default();
   write(
      &project,
      format!("{base}\n{extra}\npreferences: {}\n", preferences.join(", ")),
   )?;
   if !has_index(&home) {
      drop(
         Command::new(&cabal)
            .arg("update")
            .env("CABAL_DIR", &home)
            .output(),
      );
   }
   let flags: Vec<String> = options
      .get("flags")
      .and_then(Value::as_array)
      .map(|listed| {
         listed
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect()
      })
      .unwrap_or_default();
   let output = Command::new(&cabal)
      .arg("freeze")
      .arg(format!("--project-dir={}", work.display()))
      .arg("-w")
      .arg(&ghc)
      .args(["--disable-tests", "--disable-benchmarks"])
      .args(&flags)
      .env("CABAL_DIR", &home)
      .output()?;
   let frozen = if output.status.success() {
      read_to_string(work.join("cabal.project.freeze")).unwrap_or_default()
   } else {
      String::new()
   };
   remove_dir_all(&top)?;
   if !output.status.success() {
      let stderr = String::from_utf8_lossy(&output.stderr);
      let lines: Vec<&str> = stderr.lines().collect();
      let kept = lines.len().saturating_sub(8);
      return Err(io::Error::other(format!(
         "cabal freeze: {}",
         lines[kept..].join("\n")
      )));
   }
   let pinned = Regex::new(r"any\.([A-Za-z0-9-]+) ==([0-9.]+)").map_err(io::Error::other)?;
   let mut picks = Vec::new();
   for hit in pinned.captures_iter(&frozen) {
      let (Some(name), Some(version)) = (hit.get(1), hit.get(2)) else {
         continue;
      };
      let package = PackageId::new(name.as_str(), version.as_str())?;
      if picks.contains(&package) {
         continue;
      }
      picks.push(package);
   }
   Ok(picks)
}

struct Locked {
   key:    String,
   fields: BTreeMap<String, String>,
}

/// The entries of the set the package at `src` needs.
pub fn lock(src: &Path, sub: &str, options: &Value, old: &Entries) -> io::Result<Entries> {
   let picks = solve(src, sub, options, old)?;
   let locked = parallel(&picks, |pick| {
      let id = pick.to_string();
      if let Some(fields) = old.get(&id) {
         return Ok(Some(Locked {
            key:    id,
            fields: fields.clone(),
         }));
      }
      let response = http::cached(
         &format!("{HACKAGE}/{id}/revisions/"),
         Duration::from_secs(600),
      )?;
      if response.status != 200 {
         return Ok(None);
      }
      let revisions = response.json(&id)?;
      let Some(last) = revisions.as_array().and_then(|listed| listed.last()) else {
         return Ok(None);
      };
      let number = last
         .get("number")
         .map(|value| {
            value
               .as_u64()
               .map_or_else(|| value.to_string(), |num| num.to_string())
         })
         .unwrap_or_default();
      nix::say(&format!("  {id} r{number}"));
      let mut fields = BTreeMap::new();
      fields.insert(
         "src".to_owned(),
         nix::prefetch_file(&format!("{HACKAGE}/{id}/{id}.tar.gz"))?.hash,
      );
      fields.insert("rev".to_owned(), number);
      fields.insert(
         "cabal".to_owned(),
         nix::sri(
            last
               .get("sha256")
               .and_then(Value::as_str)
               .unwrap_or_default(),
         )?,
      );
      Ok(Some(Locked { key: id, fields }))
   })?;
   Ok(locked
      .into_iter()
      .flatten()
      .map(|entry| (entry.key, entry.fields))
      .collect())
}
