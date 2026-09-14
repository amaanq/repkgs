use std::{
   collections::BTreeMap,
   fs::read_to_string,
   io,
   path::Path,
};

use serde::Deserialize;

use crate::{
   attrs::Attrs,
   exports::Exports,
};

#[derive(Deserialize)]
struct Entry {
   pkg:   String,
   #[serde(default)]
   env:   BTreeMap<String, String>,
   #[serde(default)]
   tags:  Vec<String>,
   #[serde(default)]
   flags: Vec<String>,
}

type Tables = BTreeMap<String, BTreeMap<String, Entry>>;

const LOCKS: [(&str, &str); 4] = [
   ("cargo", "Cargo.lock"),
   ("go", "go.sum"),
   ("python", "uv.lock"),
   ("gems", "Gemfile.lock"),
];

fn tables(attrs: &Attrs) -> io::Result<Tables> {
   let tree = attrs
      .probe_inputs
      .first()
      .ok_or_else(|| io::Error::other("the builder tree is not among probeInputs"))?;
   Ok(serde_json::from_str(&read_to_string(
      Path::new(tree).join("sys-libs.json"),
   )?)?)
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
      let Some(dep) = deps.iter().find(|dep| dep.name == entry.pkg) else {
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

fn locked(ecosystem: &str, text: &str) -> Vec<String> {
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

// our packages the lock files in `dir` can link, sorted, what [pin] sys should
// say
pub fn wanted(attrs: &Attrs, dir: &Path) -> io::Result<Vec<String>> {
   let tables = tables(attrs)?;
   let mut found = Vec::new();
   for (ecosystem, file) in LOCKS {
      let Ok(text) = read_to_string(dir.join(file)) else {
         continue;
      };
      let names = locked(ecosystem, &text);
      if let Some(table) = tables.get(ecosystem) {
         for (name, entry) in table {
            if names.contains(name) && !found.contains(&entry.pkg) {
               found.push(entry.pkg.clone());
            }
         }
      }
   }
   found.sort();
   Ok(found)
}

// the lock can link libraries [pin] sys does not name, the pin is stale. None
// means no sources.toml, dependencies are by hand
pub fn check(attrs: &Attrs, dir: &Path, sys: Option<&[String]>) -> io::Result<()> {
   let Some(pinned) = sys else {
      return Ok(());
   };
   let missing: Vec<String> = wanted(attrs, dir)?
      .into_iter()
      .filter(|name| !pinned.contains(name))
      .collect();
   if missing.is_empty() {
      return Ok(());
   }
   Err(io::Error::other(format!(
      "sys-libs: the lock can link {}, missing from [pin] sys in sources.toml. `repkgs update \
       rehash <pkg>` rewrites it",
      missing.join(", ")
   )))
}
