use std::{
   collections::BTreeMap,
   io,
   path::Path,
};

use mummer::syslibs::{
   self,
   CargoBuild,
   Tables,
   load,
};

use crate::{
   attrs::Attrs,
   exports::Exports,
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
pub fn active(attrs: &Attrs, ecosystem: &str, deps: &[Exports]) -> io::Result<Active> {
   let tables = tables(attrs)?;
   let table = tables
      .get(ecosystem)
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

pub fn wanted_for(attrs: &Attrs, ecosystem: &str, names: &[String]) -> io::Result<Vec<String>> {
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
