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
   config::BinaryFormat,
   elf::{
      defined_symbols,
      is_elf,
      sections,
   },
   image::Image,
   process::loose,
   syslibs::Ecosystem,
   tree::{
      EntryKind,
      note,
      walk,
   },
};

use crate::{
   attrs::Attrs,
   exports::Exports,
};

pub struct Active {
   pub env:   BTreeMap<String, String>,
   pub tags:  Vec<String>,
   pub flags: BTreeMap<String, String>,
}

// what the dependencies' bindings of one ecosystem turn on, {root} filled in
pub fn active(attrs: &Attrs, ecosystem: Ecosystem, deps: &[Exports]) -> Active {
   let mut found = Active {
      env:   BTreeMap::new(),
      tags:  Vec::new(),
      flags: BTreeMap::new(),
   };
   for dep in deps {
      let Some(binding) = attrs
         .sys_libs
         .get(&dep.name)
         .and_then(|bindings| bindings.get(&ecosystem))
      else {
         continue;
      };
      for (name, value) in &binding.env {
         found
            .env
            .insert(name.clone(), value.replace("{root}", &dep.root));
      }
      for tag in &binding.tags {
         if !found.tags.contains(tag) {
            found.tags.push(tag.clone());
         }
      }
      for (locked, flags) in &binding.flags {
         let rendered = flags
            .iter()
            .map(|flag| flag.replace("{root}", &dep.root))
            .collect::<Vec<String>>();
         found.flags.insert(locked.clone(), rendered.join(" "));
      }
   }
   found
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
      let searched = loose(&read(path)?).lines().any(|line| {
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
