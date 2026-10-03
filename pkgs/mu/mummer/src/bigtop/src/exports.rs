use std::{
   collections::BTreeMap,
   env::var,
   fs::{
      DirEntry,
      read_dir,
      read_to_string,
   },
   io::{
      self,
      Read as _,
      Write as _,
   },
   path::{
      Path,
      PathBuf,
   },
};

use mummer::{
   files::write_file,
   store::STORE_DIR,
};
use serde::{
   Deserialize,
   Serialize,
};
use serde_json::{
   Map,
   Value,
};

use crate::{
   attrs::Extensions,
   targets::Cpu,
};

record! {
   #[derive(Default)]
   pub Stored {
      optional {
         name ("name"): String,
         include_dirs ("includeDirs"): Vec<String>,
         lib_dirs ("libDirs"): Vec<String>,
         libs ("libs"): Vec<String>,
         pkgconfig_dirs ("pkgconfigDirs"): Vec<String>,
         aclocal_dirs ("aclocalDirs"): Vec<String>,
         env ("env"): BTreeMap<String, String>,
         propagate ("propagate"): Vec<String>,
         dll_dirs ("dllDirs"): Vec<String>,
      }
   }
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Exports {
   pub name:           String,
   pub include_dirs:   Vec<String>,
   pub lib_dirs:       Vec<String>,
   pub libs:           Vec<String>,
   pub pkgconfig_dirs: Vec<String>,
   pub aclocal_dirs:   Vec<String>,
   pub env:            BTreeMap<String, String>,
   pub propagate:      Vec<String>,
   #[serde(default, skip_serializing_if = "Vec::is_empty")]
   pub dll_dirs:       Vec<String>,
   pub root:           String,
}

#[derive(Deserialize)]
pub struct Request {
   pub name:    String,
   pub ext:     Extensions,
   #[serde(default)]
   pub exports: Value,
   #[serde(default)]
   pub deps:    Vec<String>,
}

fn existing(root: &Path, candidates: &[&str]) -> Vec<String> {
   candidates
      .iter()
      .filter(|relative| root.join(relative).exists())
      .map(|relative| (*relative).to_owned())
      .collect()
}

fn package_name(root: &Path) -> String {
   let base = root
      .file_name()
      .map(|name| name.to_string_lossy().into_owned())
      .unwrap_or_default();
   let name = base.get(33..).unwrap_or_default();
   for cpu in Cpu::ALL.map(Cpu::name) {
      let marker = format!("-{cpu}-");
      if let Some(position) = name.rfind(&marker) {
         let tail = &name[position + marker.len()..];
         if !tail.is_empty()
            && tail
               .bytes()
               .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
         {
            return name[..position].to_owned();
         }
      }
   }
   name.to_owned()
}

fn linkable_libraries(root: &Path, extensions: &Extensions) -> io::Result<Vec<String>> {
   let suffix = extensions.import.as_ref().unwrap_or(&extensions.shared);
   let mut names: Vec<(String, String)> = Vec::new();
   let entries = match read_dir(root.join("lib")) {
      Ok(entries) => entries,
      Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
      Err(error) => return Err(error),
   };
   for result in entries {
      let child = result?;
      if child.file_type()?.is_dir() {
         continue;
      }
      let file = child.file_name().to_string_lossy().into_owned();
      if let Some(stem) = file
         .strip_prefix("lib")
         .and_then(|rest| rest.strip_suffix(suffix))
      {
         names.push((file.clone(), stem.to_owned()));
      }
   }
   names.sort_unstable();
   Ok(names.into_iter().map(|(_, stem)| stem).collect())
}

fn read_stored(root: &Path) -> io::Result<Stored> {
   match read_to_string(root.join("exports.json")) {
      Ok(text) => serde_json::from_str(&text).map_err(io::Error::other),
      Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Stored::default()),
      Err(error) => Err(error),
   }
}

pub fn record_dll_dirs(root: &Path, directories: Vec<String>) -> io::Result<()> {
   let mut exports = read_stored(root)?;
   exports.dll_dirs = Some(directories);
   let mut rendered = serde_json::to_vec_pretty(&exports)?;
   rendered.push(b'\n');
   write_file(&root.join("exports.json"), &rendered, None)
}

pub fn exports_of(root: &Path) -> io::Result<Exports> {
   let stored = read_stored(root)?;
   let root_text = root.to_string_lossy().into_owned();
   let nix_store = root.parent().unwrap_or(root).to_string_lossy();
   let expand = |text: String| {
      text
         .replace("{root}", &root_text)
         .replace("{store}", &nix_store)
   };

   Ok(Exports {
      name:           stored.name.unwrap_or_else(|| package_name(root)),
      include_dirs:   stored
         .include_dirs
         .unwrap_or_else(|| existing(root, &["include"])),
      lib_dirs:       stored.lib_dirs.unwrap_or_else(|| existing(root, &["lib"])),
      libs:           stored.libs.unwrap_or_default(),
      pkgconfig_dirs: stored
         .pkgconfig_dirs
         .unwrap_or_else(|| existing(root, &["lib/pkgconfig", "share/pkgconfig"])),
      aclocal_dirs:   stored
         .aclocal_dirs
         .unwrap_or_else(|| existing(root, &["share/aclocal"])),
      env:            stored
         .env
         .unwrap_or_default()
         .into_iter()
         .map(|(name, value)| (name, expand(value)))
         .collect(),
      propagate:      stored.propagate.unwrap_or_default(),
      dll_dirs:       stored
         .dll_dirs
         .unwrap_or_default()
         .into_iter()
         .map(expand)
         .collect(),
      root:           root_text,
   })
}

pub fn closure(roots: &[String]) -> io::Result<Vec<Exports>> {
   let mut done: Vec<Exports> = Vec::new();
   let mut todo: Vec<String> = roots.to_vec();
   let mut next = 0;
   while next < todo.len() {
      let root = todo[next].clone();
      next += 1;
      if done.iter().any(|entry| entry.root == root) {
         continue;
      }
      let entry = exports_of(Path::new(&root))?;
      todo.extend(entry.propagate.iter().cloned());
      done.push(entry);
   }
   Ok(done)
}

pub fn lib_dirs(interp: &Path, roots: &[String]) -> io::Result<Vec<String>> {
   let mut dirs = vec![
      interp
         .parent()
         .unwrap_or(interp)
         .to_string_lossy()
         .into_owned(),
   ];
   for entry in closure(roots)? {
      dirs.extend(
         entry
            .lib_dirs
            .iter()
            .map(|rel| format!("{}/{rel}", entry.root)),
      );
   }
   Ok(dirs)
}

pub fn print(roots: &[String], transitive: bool) -> io::Result<()> {
   let entries = if transitive {
      closure(roots)?
   } else {
      roots
         .iter()
         .map(|root| exports_of(Path::new(root)))
         .collect::<io::Result<Vec<_>>>()?
   };
   let mut stdout = io::stdout().lock();
   serde_json::to_writer(&mut stdout, &entries)?;
   writeln!(stdout)?;
   Ok(())
}

pub fn write(root: &Path) -> io::Result<()> {
   let mut text = String::new();
   io::stdin().read_to_string(&mut text)?;
   write_request(root, &serde_json::from_str::<Request>(&text)?)
}

pub fn write_request(root: &Path, request: &Request) -> io::Result<()> {
   let own = match request.exports {
      Value::Bool(false) => {
         [
            "includeDirs",
            "libDirs",
            "libs",
            "pkgconfigDirs",
            "aclocalDirs",
         ]
         .into_iter()
         .map(|field| (field.to_owned(), Value::Array(Vec::new())))
         .collect()
      },
      Value::Null => Map::new(),
      Value::Object(ref fields) => fields.clone(),
      Value::Bool(true) | Value::Number(_) | Value::String(_) | Value::Array(_) => {
         return Err(io::Error::other("exports must be a record or false"));
      },
   };
   let mut exports = match serde_json::to_value(exports_of(root)?)? {
      Value::Object(fields) => fields,
      Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) | Value::Array(_) => {
         return Err(io::Error::other("exports did not serialise as a record"));
      },
   };
   exports.remove("root");
   exports.insert(
      "libs".to_owned(),
      serde_json::to_value(linkable_libraries(root, &request.ext)?)?,
   );
   for (key, value) in own {
      exports.insert(key, value);
   }
   exports.insert("name".to_owned(), Value::String(request.name.clone()));
   let pkgconfig_dirs = exports
      .get("pkgconfigDirs")
      .and_then(Value::as_array)
      .map(|dirs| {
         dirs
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect::<Vec<_>>()
      })
      .unwrap_or_default();
   let mut propagate = exports
      .get("propagate")
      .and_then(Value::as_array)
      .map(|list| {
         list
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect::<Vec<_>>()
      })
      .unwrap_or_default();
   for dep in required_deps(root, &request.deps, &pkgconfig_dirs)? {
      if !propagate.contains(&dep) {
         propagate.push(dep);
      }
   }
   exports.insert(
      "propagate".to_owned(),
      Value::Array(propagate.into_iter().map(Value::String).collect()),
   );

   if let Some(directories) = exports.remove("dllDirs") {
      let absolute: Vec<String> = serde_json::from_value(directories)?;

      if !absolute.is_empty() {
         let root_text = root.to_string_lossy();
         let nix_store = var("NIX_STORE").unwrap_or_else(|_| STORE_DIR.to_owned());
         let relative: Vec<String> = absolute
            .iter()
            .map(|directory| {
               directory
                  .replace(root_text.as_ref(), "{root}")
                  .replace(&nix_store, "{store}")
            })
            .collect();
         exports.insert("dllDirs".to_owned(), serde_json::to_value(relative)?);
      }
   }

   let mut rendered = serde_json::to_vec_pretty(&Value::Object(exports))?;
   rendered.push(b'\n');
   write_file(&root.join("exports.json"), &rendered, None)
}

fn identifier(token: &str) -> bool {
   let mut bytes = token.bytes();
   bytes
      .next()
      .is_some_and(|first| first.is_ascii_alphabetic() || first == b'_')
      && bytes.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'+' | b'-'))
}

fn requires(text: &str, wanted: &mut Vec<String>) {
   for line in text.lines() {
      let Some(after) = line.strip_prefix("Requires") else {
         continue;
      };
      let Some(value) = after
         .strip_prefix(".private")
         .unwrap_or(after)
         .strip_prefix(':')
      else {
         continue;
      };
      for token in value.split(|character: char| character == ',' || character.is_whitespace()) {
         if identifier(token) {
            wanted.push(format!("{}.pc", token.to_ascii_lowercase()));
         }
      }
   }
}

fn find_dependencies(text: &str, wanted: &mut Vec<String>) {
   let mut rest = text;
   while let Some(position) = rest.find("find_dependency") {
      rest = &rest[position + "find_dependency".len()..];
      let trimmed = rest.trim_start();
      let Some(inner) = trimmed.strip_prefix('(') else {
         continue;
      };
      let name: String = inner
         .trim_start()
         .chars()
         .take_while(|character| character.is_alphanumeric() || matches!(character, '_' | '-'))
         .collect();
      if !name.is_empty() {
         wanted.push(name.to_ascii_lowercase());
      }
   }
}

fn walk(directory: &Path, suffix: &str, found: &mut Vec<PathBuf>) -> io::Result<()> {
   let entries = match read_dir(directory) {
      Ok(entries) => entries,
      Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
      Err(error) => return Err(error),
   };
   let mut children: Vec<_> = entries.collect::<io::Result<_>>()?;
   children.sort_by_key(DirEntry::file_name);
   for child in children {
      let path = child.path();
      if path.is_dir() {
         walk(&path, suffix, found)?;
      } else if path.to_string_lossy().ends_with(suffix) {
         found.push(path);
      }
   }
   Ok(())
}

fn required_deps(
   out: &Path,
   deps: &[String],
   pkgconfig_dirs: &[String],
) -> io::Result<Vec<String>> {
   let mut wanted = Vec::new();
   for directory in pkgconfig_dirs {
      let mut entries = match read_dir(out.join(directory)) {
         Ok(entries) => entries.collect::<io::Result<Vec<_>>>()?,
         Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
         Err(error) => return Err(error),
      };
      entries.sort_by_key(DirEntry::file_name);
      for entry in entries {
         let path = entry.path();
         if path.extension().is_some_and(|extension| extension == "pc") && !path.is_dir() {
            requires(&read_to_string(path)?, &mut wanted);
         }
      }
   }
   let mut configs = Vec::new();
   walk(&out.join("lib/cmake"), ".cmake", &mut configs)?;
   for config in configs {
      find_dependencies(&read_to_string(config)?, &mut wanted);
   }
   let out_text = out.to_string_lossy();
   let roots: Vec<&String> = deps.iter().filter(|root| **root != out_text).collect();
   if roots.is_empty() || wanted.is_empty() {
      return Ok(Vec::new());
   }
   let mut providers = Vec::new();
   for root in roots {
      for first in list(Path::new(root))? {
         for second in list(&first)? {
            let Some(kind) = second.file_name().map(|name| name.to_string_lossy()) else {
               continue;
            };
            for third in list(&second)? {
               let name = third
                  .file_name()
                  .map(|name| name.to_string_lossy().to_ascii_lowercase())
                  .unwrap_or_default();
               let offered = (kind == "pkgconfig"
                  && Path::new(&name)
                     .extension()
                     .is_some_and(|extension| extension == "pc"))
                  || kind == "cmake";
               if offered && wanted.contains(&name) {
                  providers.push(root.clone());
               }
            }
         }
      }
   }
   providers.sort_unstable();
   providers.dedup();
   Ok(providers)
}

fn list(directory: &Path) -> io::Result<Vec<PathBuf>> {
   let entries = match read_dir(directory) {
      Ok(entries) => entries,
      Err(error)
         if matches!(
            error.kind(),
            io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
         ) =>
      {
         return Ok(Vec::new());
      },
      Err(error) => return Err(error),
   };
   let mut paths: Vec<_> = entries
      .map(|result| result.map(|child| child.path()))
      .collect::<io::Result<_>>()?;
   paths.sort_unstable();
   Ok(paths)
}
