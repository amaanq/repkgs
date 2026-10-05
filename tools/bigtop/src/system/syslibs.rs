use std::{
   collections::{
      BTreeMap,
      HashSet,
   },
   fs::read,
   io,
   path::Path,
};

use mummer::{
   elf::{
      defined_symbols,
      is_elf,
      sections,
   },
   image::Image,
   syslibs::{
      self,
      CargoBuild,
      Ecosystem,
      Tables,
      load,
   },
   tree::{
      EntryKind,
      note,
      walk,
   },
};

use crate::{
   attrs::Attrs,
   exports::Exports,
   hardening::BinaryFormat,
};

pub fn tables(attrs: &Attrs) -> io::Result<Tables> {
   let tree = attrs
      .probe_inputs
      .first()
      .ok_or_else(|| io::Error::other("the builder tree is not among probeInputs"))?;
   load(&Path::new(tree).join("sys-libs.json"))
}

pub struct Active {
   pub env:   BTreeMap<String, String>,
   pub tags:  Vec<String>,
   pub flags: BTreeMap<String, String>,
}

// the table entries of one ecosystem whose library is among the dependencies,
// {root} filled in
pub fn active(attrs: &Attrs, ecosystem: Ecosystem, deps: &[Exports]) -> io::Result<Active> {
   let tables = tables(attrs)?;
   let table = tables
      .get(&ecosystem)
      .ok_or_else(|| io::Error::other(format!("sys-libs has no {ecosystem} table")))?;
   let mut found = Active {
      env:   BTreeMap::new(),
      tags:  Vec::new(),
      flags: BTreeMap::new(),
   };
   for (locked, entry) in table {
      let Some(dep) = deps
         .iter()
         .find(|dep| entry.pkg.as_ref() == Some(&dep.name))
      else {
         continue;
      };
      for (name, value) in &entry.env {
         found
            .env
            .insert(name.clone(), value.replace("{root}", &dep.root));
      }
      for tag in &entry.tags {
         if !found.tags.contains(tag) {
            found.tags.push(tag.clone());
         }
      }
      if !entry.flags.is_empty() {
         let rendered: Vec<String> = entry
            .flags
            .iter()
            .map(|flag| flag.replace("{root}", &dep.root))
            .collect();
         found.flags.insert(locked.clone(), rendered.join(" "));
      }
   }
   Ok(found)
}

pub fn wanted_for(
   attrs: &Attrs,
   ecosystem: Ecosystem,
   names: &[String],
) -> io::Result<Vec<String>> {
   Ok(syslibs::wanted_for(&tables(attrs)?, ecosystem, names))
}

// our packages the lock files in `dir` can link, sorted, what act.lock's sys
// should say
pub fn wanted(attrs: &Attrs, dir: &Path, cargo: Option<&CargoBuild>) -> io::Result<Vec<String>> {
   syslibs::wanted(&tables(attrs)?, dir, cargo)
}

// the lock can link libraries act.lock's sys does not name, the pin is stale.
// None means no act.toml, dependencies are by hand
pub fn check(
   attrs: &Attrs,
   dir: &Path,
   sys: Option<&[String]>,
   cargo: Option<&CargoBuild>,
) -> io::Result<()> {
   check_names(&wanted(attrs, dir, cargo)?, sys, "rehash")
}

pub fn check_names(wanted: &[String], sys: Option<&[String]>, command: &str) -> io::Result<()> {
   let Some(pinned) = sys else {
      return Ok(());
   };
   let missing: Vec<&str> = wanted
      .iter()
      .filter(|name| !pinned.contains(name))
      .map(String::as_str)
      .collect();
   if missing.is_empty() {
      return Ok(());
   }
   Err(io::Error::other(format!(
      "sys-libs: the lock can link {}, missing from act.lock's sys. `motley update {command} \
       <pkg>` rewrites it",
      missing.join(", ")
   )))
}

/// Every `sys` package among the dependencies must be what the outputs link,
/// since a vendored copy defines the functions the system library exports.
pub fn linked(
   sys: &[String],
   deps: &[Exports],
   out: &Path,
   build: &Path,
   format: BinaryFormat,
) -> io::Result<()> {
   if sys.is_empty() {
      return Ok(());
   }
   if format != BinaryFormat::Elf {
      note("sys", "linkage not checked, bigtop reads only ELF symbols");
      return Ok(());
   }
   let symbols = |path: &Path, kind: u32| -> io::Result<Option<Vec<String>>> {
      let image = Image { bytes: read(path)? };
      if !is_elf(&image) {
         return Ok(None);
      }
      Ok(Some(defined_symbols(&image, &sections(&image)?, kind)?))
   };
   let mut outputs = Vec::new();
   for entry in walk(out)?
      .into_iter()
      .filter(|entry| entry.kind == EntryKind::File)
   {
      if let Some(defined) = symbols(Path::new(&entry.path), 2)? {
         outputs.push((entry.rel, defined));
      }
   }
   let mut vendored = Vec::new();
   for name in sys {
      let Some(dep) = deps.iter().find(|dep| dep.name == *name) else {
         continue;
      };
      let mut exported = HashSet::new();
      for directory in &dep.lib_dirs {
         for entry in walk(&Path::new(&dep.root).join(directory))?
            .into_iter()
            .filter(|entry| entry.kind == EntryKind::File)
         {
            exported.extend(symbols(Path::new(&entry.path), 11)?.unwrap_or_default());
         }
      }
      let mut found = Vec::new();
      for output in &outputs {
         let copied = output
            .1
            .iter()
            .filter(|symbol| exported.contains(symbol.as_str()))
            .take(3)
            .map(String::as_str)
            .collect::<Vec<_>>();
         if !copied.is_empty() {
            found.push(format!(
               "{} builds in its own {name} ({})",
               output.0,
               copied.join(", ")
            ));
         }
      }
      if !found.is_empty() && !searched_by_build_script(build, &dep.root)? {
         vendored.extend(found);
      }
   }
   if vendored.is_empty() {
      return Ok(());
   }
   Err(io::Error::other(format!(
      "sys lists packages the outputs do not link:\n{}",
      vendored.join("\n")
   )))
}

/// A cargo build script that put the dependency on the link search path,
/// as `pkg-config` does for a crate that links the system library statically.
fn searched_by_build_script(build: &Path, root: &str) -> io::Result<bool> {
   if !build.is_dir() {
      return Ok(false);
   }
   for entry in walk(build)? {
      let path = Path::new(&entry.path);
      let script_output = entry.kind == EntryKind::File
         && path.file_name().is_some_and(|name| name == "output")
         && path
            .parent()
            .and_then(Path::parent)
            .and_then(Path::file_name)
            .is_some_and(|name| name == "build");
      if !script_output {
         continue;
      }
      let searched = String::from_utf8_lossy(&read(path)?).lines().any(|line| {
         line
            .strip_prefix("cargo::rustc-link-search=")
            .or_else(|| line.strip_prefix("cargo:rustc-link-search="))
            .map(|value| value.split_once('=').map_or(value, |pair| pair.1))
            .is_some_and(|directory| directory.starts_with(root))
      });
      if searched {
         return Ok(true);
      }
   }
   Ok(false)
}
