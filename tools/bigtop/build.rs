//! The source fingerprints output cache keys are made of, one over the code
//! every build runs and one per build-system module over itself and the
//! sibling modules it names.

use std::{
   collections::{
      BTreeMap,
      BTreeSet,
   },
   env::var_os,
   fmt::Write as _,
   fs::{
      read,
      read_dir,
      read_to_string,
      write,
   },
   io,
   path::{
      Path,
      PathBuf,
   },
};

use blake3::Hasher;

/// The `system` modules every build system goes through, shared code.
const PLUMBING: [&str; 4] = ["mod", "command", "dispatch", "syslibs"];

fn sources(directory: &Path, found: &mut Vec<PathBuf>) -> io::Result<()> {
   for entry in read_dir(directory)? {
      let path = entry?.path();
      if path.is_dir() {
         sources(&path, found)?;
      } else if path.extension().is_some_and(|extension| extension == "rs") {
         found.push(path);
      }
   }
   Ok(())
}

fn digest(files: &BTreeSet<PathBuf>, root: &Path) -> io::Result<String> {
   let mut hasher = Hasher::new();
   for file in files {
      let relative = file.strip_prefix(root).unwrap_or(file);
      let bytes = read(file)?;
      let length = u64::try_from(bytes.len()).map_err(io::Error::other)?;
      hasher.update(relative.as_os_str().as_encoded_bytes());
      hasher.update(&length.to_le_bytes());
      hasher.update(&bytes);
   }
   Ok(hasher.finalize().to_hex().to_string())
}

/// Whether `text` has a path through `module`, a `module::` not inside a
/// longer name or opening a string, such as the `"cargo::rustc-link-search="`
/// build-script directive syslibs reads.
fn names(text: &str, module: &str) -> bool {
   let needle = format!("{module}::");
   text.match_indices(&needle).any(|(at, _)| {
      text[..at]
         .chars()
         .next_back()
         .is_none_or(|before| !before.is_alphanumeric() && !matches!(before, '_' | '"'))
   })
}

fn environment(name: &str) -> io::Result<PathBuf> {
   var_os(name)
      .map(PathBuf::from)
      .ok_or_else(|| io::Error::other(format!("{name} is not set")))
}

/// The sources of the crates bigtop links, the mummer library's modules and
/// tightrope, with the lock file that pins the rest.
fn linked(library: &Path, protocol: &Path, lock: PathBuf) -> io::Result<Vec<PathBuf>> {
   let mut found = vec![library.join("lib.rs")];
   for line in read_to_string(library.join("lib.rs"))?.lines() {
      let Some(name) = line
         .trim()
         .strip_prefix("pub mod ")
         .and_then(|rest| rest.strip_suffix(';'))
      else {
         continue;
      };
      let file = library.join(format!("{name}.rs"));
      if file.is_file() {
         found.push(file);
      }
      let nested = library.join(name);
      if nested.is_dir() {
         sources(&nested, &mut found)?;
      }
   }
   sources(protocol, &mut found)?;
   found.push(lock);
   Ok(found)
}

fn main() -> io::Result<()> {
   let crate_dir = environment("CARGO_MANIFEST_DIR")?;
   let tools = crate_dir
      .parent()
      .ok_or_else(|| io::Error::other("bigtop has no workspace above it"))?
      .to_path_buf();
   let src = crate_dir.join("src");
   let system = src.join("system");
   let library = tools.join("bootstrap/mummer/src");
   let protocol = tools.join("bootstrap/tightrope/src");
   let lock = tools.join("Cargo.lock");
   for watched in [&src, &library, &protocol, &lock] {
      println!("cargo::rerun-if-changed={}", watched.display());
   }

   let mut found = Vec::new();
   sources(&src, &mut found)?;
   let module_of = |path: &Path| {
      (path.parent() == Some(system.as_path()))
         .then(|| path.file_stem()?.to_str())
         .flatten()
         .filter(|stem| !PLUMBING.contains(stem))
         .map(str::to_owned)
   };
   let mut modules = BTreeMap::new();
   let mut shared = BTreeSet::new();
   for path in found {
      match module_of(&path) {
         Some(name) => {
            modules.insert(name, path);
         },
         None => {
            shared.insert(path);
         },
      }
   }
   let texts = modules
      .iter()
      .map(|(name, path)| Ok((name.as_str(), read_to_string(path)?)))
      .collect::<io::Result<BTreeMap<_, _>>>()?;

   // dispatch records the modules it calls into, and nothing records one
   // shared code calls directly, so that one counts as shared
   let dispatcher = system.join("dispatch.rs");
   let mut folded = Vec::new();
   for file in shared.iter().filter(|file| **file != dispatcher) {
      let text = read_to_string(file)?;
      folded.extend(
         modules
            .iter()
            .filter(|&(name, _)| names(&text, name))
            .map(|(_, path)| path.clone()),
      );
   }
   shared.extend(folded);

   shared.extend(linked(&library, &protocol, lock)?);

   let mut generated = format!(
      "/// Over the code every build runs.\npub const SHARED: &str = \"{}\";\n\n/// Each \
       build-system module over itself and the siblings it names.\npub const MODULES: &[(&str, \
       &str)] = &[\n",
      digest(&shared, &tools)?
   );
   for name in modules.keys() {
      let mut closure = BTreeSet::from([name.as_str()]);
      let mut pending = vec![name.as_str()];
      while let Some(current) = pending.pop() {
         let text = texts.get(current).map_or("", String::as_str);
         for other in modules.keys() {
            if !closure.contains(other.as_str()) && names(text, other) {
               closure.insert(other);
               pending.push(other);
            }
         }
      }
      let files = closure
         .iter()
         .filter_map(|member| modules.get(*member).cloned())
         .collect::<BTreeSet<_>>();
      writeln!(
         generated,
         "   (\"{name}\", \"{}\"),",
         digest(&files, &tools)?
      )
      .map_err(io::Error::other)?;
   }
   generated.push_str("];\n");
   write(environment("OUT_DIR")?.join("fingerprints.rs"), generated)
}
