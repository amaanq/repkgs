//! The repo-wide `locks/<eco>.toml`, its format and filling it from packages'
//! `[locks]`.
//!
//! One sorted `"<key>" = { ... }` line per entry, so additions merge as text
//! (merge=union in .gitattributes).

use std::{
   collections::BTreeMap,
   env::var,
   ffi::OsStr,
   fs::{
      create_dir_all,
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
   hackage::PackageId,
   syslibs::wanted_for,
};

use crate::{
   go,
   hackage,
   luarocks,
   nix,
   pipeline::{
      self,
      root,
   },
   sources::Package,
};

pub type Entries = BTreeMap<String, BTreeMap<String, String>>;

pub struct Used {
   pub eco:  String,
   pub keys: Vec<String>,
}

/// `$SPIELER_LOCKS`, default `<root>/locks`.
pub fn dir() -> PathBuf {
   var("SPIELER_LOCKS").map_or_else(|_unset| root().join("locks"), PathBuf::from)
}

fn parse(text: &str, eco: &str) -> io::Result<Entries> {
   let doc: toml::Table = toml::from_str(text).map_err(io::Error::other)?;
   let Some(table) = doc.get(eco).and_then(toml::Value::as_table) else {
      return Ok(Entries::new());
   };
   let mut entries = Entries::new();
   for (key, fields) in table {
      let mut found = BTreeMap::new();
      if let Some(inline) = fields.as_table() {
         for (field, item) in inline {
            found.insert(
               field.clone(),
               item
                  .as_str()
                  .map_or_else(|| item.to_string(), str::to_owned),
            );
         }
      }
      entries.insert(key.clone(), found);
   }
   Ok(entries)
}

/// Entries of `<dir>/<eco>.toml`, empty when absent.
pub fn read(directory: &Path, eco: &str) -> io::Result<Entries> {
   let file = directory.join(format!("{eco}.toml"));
   if !file.exists() {
      return Ok(Entries::new());
   }
   parse(&read_to_string(file)?, eco)
}

fn render(eco: &str, entries: &Entries) -> String {
   let width = entries.keys().map(|key| key.len() + 2).max().unwrap_or(0);
   let lines = entries
      .iter()
      .map(|(key, fields)| {
         let rendered = fields
            .iter()
            .map(|(field, item)| format!("{field} = \"{item}\""))
            .collect::<Vec<_>>()
            .join(", ");
         format!("{:<width$} = {{ {rendered} }}\n", format!("\"{key}\""))
      })
      .collect::<Vec<_>>()
      .concat();
   format!("[{eco}]\n{lines}")
}

/// Replaces `<dir>/<eco>.toml` with `entries` in normal form.
pub fn write_entries(directory: &Path, eco: &str, entries: &Entries) -> io::Result<()> {
   create_dir_all(directory)?;
   write(directory.join(format!("{eco}.toml")), render(eco, entries))
}

/// A union merge can leave an entry twice with different padding, which TOML
/// rejects, so those lines are read one by one and equal repeats dropped.
pub fn normalize(file: &Path) -> io::Result<()> {
   let eco = file
      .file_stem()
      .map(|stem| stem.to_string_lossy().into_owned())
      .unwrap_or_default();
   let raw = read_to_string(file)?;
   let entries = if let Ok(parsed) = parse(&raw, &eco) {
      parsed
   } else {
      let mut merged = Entries::new();
      for line in raw.lines().filter(|line| line.starts_with('"')) {
         for (key, fields) in parse(&format!("[{eco}]\n{line}"), &eco)? {
            match merged.get(&key) {
               Some(known) if *known != fields => {
                  return Err(io::Error::other(format!(
                     "{}: {key} has two different entries",
                     file.display()
                  )));
               },
               _ => {
                  merged.insert(key, fields);
               },
            }
         }
      }
      merged
   };
   let normal = render(&eco, &entries);
   if normal != raw {
      write(file, normal)?;
   }
   Ok(())
}

/// The package's source at its current pin, a built derivation or an in-tree
/// path (source = ./src).
pub fn source_of(package: &Package) -> io::Result<PathBuf> {
   let built = nix::capture("nix-build", [
      root().as_os_str(),
      OsStr::new("-A"),
      OsStr::new(&format!("{}.src", package.name)),
      OsStr::new("--no-out-link"),
   ])?;
   if built.status == 0_i32 {
      return Ok(PathBuf::from(built.stdout.trim()));
   }
   let path = PathBuf::from(nix::eval_raw(&root(), &format!("{}.src", package.name))?);
   if !path.exists() {
      let tail: Vec<&str> = built.stderr.lines().collect();
      let kept = tail.len().saturating_sub(3);
      return Err(io::Error::other(format!(
         "{}.src: {}",
         package.name,
         tail[kept..].join("\n")
      )));
   }
   Ok(path)
}

/// Adds this package's dependencies ([locks] go = "<dir in source>") to
/// `<dir>/<eco>.toml` from the source at the current pin; entries already
/// present are not fetched again. Returns what this package uses, for prune.
pub fn add(package: &Package) -> io::Result<Vec<Used>> {
   if package.locks.is_empty() {
      return Ok(Vec::new());
   }
   let src = source_of(package)?;
   let mut used = Vec::new();
   for (eco, sub) in &package.locks {
      let old = read(&dir(), eco)?;
      let mine = match eco.as_str() {
         "go" => go::lock(&src.join(sub), &old)?,
         "hackage" => {
            let options = nix::eval_json(&root(), &format!("{}.spec.cabal", package.name))?;
            hackage::lock(&src, sub, &options, &old)?
         },
         "luarocks" => luarocks::lock(&src.join(sub), &old)?,
         other => {
            return Err(io::Error::other(format!("no lock step for {other}")));
         },
      };
      let mut merged = old.clone();
      for (key, fields) in &mine {
         merged.insert(key.clone(), fields.clone());
      }
      nix::say(&format!(
         "  {eco}: {} entries, {} new",
         mine.len(),
         merged.len().saturating_sub(old.len())
      ));
      write_entries(&dir(), eco, &merged)?;
      let keys: Vec<String> = mine.keys().cloned().collect();
      // no lock file in the source for prefetch to read: sys comes from the
      // solved names
      if eco == "hackage" {
         let names = keys
            .iter()
            .map(|key| key.parse::<PackageId>().map(|id| id.name().to_owned()))
            .collect::<io::Result<Vec<_>>>()?;
         pipeline::set_sys(
            package,
            &wanted_for(&pipeline::tables()?, "hackage", &names),
         )?;
      }
      used.push(Used {
         eco: eco.clone(),
         keys,
      });
   }
   Ok(used)
}

/// Rewrites each table to exactly the keys in `used`, dropping the rest.
pub fn prune(used: &[Used]) -> io::Result<()> {
   for entry in used {
      let old = read(&dir(), &entry.eco)?;
      let kept: Entries = old
         .iter()
         .filter(|pair| entry.keys.contains(pair.0))
         .map(|(key, fields)| (key.clone(), fields.clone()))
         .collect();
      nix::say(&format!(
         "{}: kept {}, dropped {}",
         entry.eco,
         kept.len(),
         old.len().saturating_sub(kept.len())
      ));
      write_entries(&dir(), &entry.eco, &kept)?;
   }
   Ok(())
}
