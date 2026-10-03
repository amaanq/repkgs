use std::{
   collections::BTreeSet,
   env::var,
   fs::{
      copy,
      create_dir_all,
      read,
      read_to_string,
      remove_dir_all,
      remove_file,
      rename,
      write,
   },
   io,
   os::unix::fs::symlink,
   path::{
      Path,
      PathBuf,
   },
};

use serde::Deserialize;
use shim::{
   elf::{
      Text,
      has_segment,
      interpreter,
      is_elf,
      needed,
      relative_from,
      runpath_text,
      segments,
   },
   image::Image,
   store::normalize,
   tree::{
      Entry,
      note,
      walk,
      writable,
   },
};

const LIBC: [&str; 7] = [
   "libc.so.6",
   "libm.so.6",
   "libdl.so.2",
   "libpthread.so.0",
   "librt.so.1",
   "libresolv.so.2",
   "libutil.so.1",
];

#[derive(Deserialize)]
struct Attrs {
   package: String,
   outputs: Outputs,
   closure: Vec<Member>,
}

#[derive(Deserialize)]
struct Outputs {
   out: String,
}

#[derive(Deserialize)]
struct Member {
   path: String,
}

struct Tree<'attrs> {
   out:     &'attrs Path,
   lib:     PathBuf,
   closure: Vec<&'attrs str>,
}

/// One output as a self-contained tree, the shape upstream binary tarballs
/// have and `prebuilt = true` unpacks. Foreign libraries are copied under
/// lib/, NEEDED and RUNPATH point there, `PT_INTERP` becomes the standard
/// absolute path, launchers give way to their programs. Strings are rewritten
/// in place and zero filled, so no store hash survives for Nix to find.
pub fn run() -> io::Result<()> {
   let attrs: Attrs = serde_json::from_str(&read_to_string(
      var("NIX_ATTRS_JSON_FILE").map_err(io::Error::other)?,
   )?)?;
   let out = Path::new(&attrs.outputs.out);
   copy_tree(Path::new(&attrs.package), out)?;
   remove_if_present(&out.join("exports.json"))?;
   if out.join("lib/debug").is_dir() {
      remove_dir_all(out.join("lib/debug"))?;
   }
   unwrap_launchers(&out.join("bin"))?;
   let tree = Tree {
      out,
      lib: out.join("lib"),
      closure: attrs
         .closure
         .iter()
         .map(|member| member.path.as_str())
         .filter(|path| *path != attrs.package)
         .collect(),
   };
   create_dir_all(&tree.lib)?;
   let mut queue: Vec<PathBuf> = walk(out)?
      .into_iter()
      .filter(|entry| entry.kind == 'f')
      .map(|entry| PathBuf::from(entry.path))
      .collect();
   let mut seen = BTreeSet::new();
   while let Some(file) = queue.pop() {
      if seen.insert(file.clone()) {
         queue.extend(relink(&tree, &file)?);
      }
   }
   let copied = walk(&tree.lib)?
      .iter()
      .filter(|entry| entry.kind == 'f' && !entry.rel.contains('/'))
      .count();
   note(
      "bundle",
      &format!("{} ELF files, {copied} in lib/", seen.len()),
   );
   Ok(())
}

/// Rewrites one file if it is a dynamic ELF, returns the libraries copied in
/// for it, which need the same treatment.
fn relink(tree: &Tree<'_>, file: &Path) -> io::Result<Vec<PathBuf>> {
   let mut image = Image { bytes: read(file)? };
   if !is_elf(&image) {
      return Ok(Vec::new());
   }
   let segments = segments(&image)?;
   if !has_segment(&segments, 2) {
      return Ok(Vec::new());
   }
   let directory = file.parent().unwrap_or(tree.out);
   let relative = relative_from(directory, &tree.lib);
   let reldir = if relative == Path::new(".") {
      String::new()
   } else {
      format!("/{}", relative.display())
   };
   let mut copied = Vec::new();
   for entry in needed(&image, &segments)? {
      let name = &entry.value;
      if resolves_inside(name, directory, tree.out) || name.contains("ld-linux") {
         continue;
      }
      let base = basename(name);
      if LIBC.contains(&base) {
         continue;
      }
      let Some(from) = tree
         .closure
         .iter()
         .map(|member| Path::new(member).join("lib").join(base))
         .find(|candidate| candidate.exists())
      else {
         if name.contains('/') {
            return Err(io::Error::other(format!(
               "bundle: {}: {name} not in the closure",
               file.display()
            )));
         }
         continue;
      };
      let placed = tree.lib.join(base);
      if !placed.exists() {
         copy(&from, &placed)?;
         writable(&placed)?;
         copied.push(placed);
      }
      if name.contains('/') {
         replace(&mut image, file, &entry, &format!("$ORIGIN{reldir}/{base}"))?;
      }
   }
   if let Some(entry) = runpath_text(&image, &segments)? {
      let mut kept: Vec<&str> = entry
         .value
         .split(':')
         .filter(|dir| !dir.is_empty() && !names_store_path(dir))
         .collect();
      let origin = format!("$ORIGIN{reldir}");
      if !kept.contains(&origin.as_str()) {
         kept.push(&origin);
      }
      replace(&mut image, file, &entry, &kept.join(":"))?;
   }
   if let Some(entry) = interpreter(&image, &segments)?
      && names_store_path(&entry.value)
   {
      replace(
         &mut image,
         file,
         &entry,
         &format!("/lib/{}", basename(&entry.value)),
      )?;
   }
   write(file, &image.bytes)?;
   Ok(copied)
}

fn replace(image: &mut Image, file: &Path, entry: &Text, value: &str) -> io::Result<()> {
   if value == entry.value {
      return Ok(());
   }
   note(
      "bundle",
      &format!("{}: {} -> {value}", file.display(), entry.value),
   );
   image
      .padded(entry.offset, entry.capacity, value.as_bytes())
      .map_err(|error| {
         io::Error::other(format!(
            "bundle: {}: {value} does not fit over {}, {error}",
            file.display(),
            entry.value
         ))
      })
}

fn copy_tree(from: &Path, to: &Path) -> io::Result<()> {
   create_dir_all(to)?;
   for entry in walk(from)? {
      let target = to.join(&entry.rel);
      match entry.kind {
         'd' => create_dir_all(&target)?,
         'l' => symlink(&entry.target, &target)?,
         _ => {
            copy(&entry.path, &target)?;
            writable(&target)?;
         },
      }
   }
   for entry in walk(to)?.iter().filter(|entry| entry.kind == 'd') {
      writable(Path::new(&entry.path))?;
   }
   writable(to)
}

fn remove_if_present(path: &Path) -> io::Result<()> {
   match remove_file(path) {
      Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
      result => result,
   }
}

/// bin/x -> launch, bin/.x the program, bin/.x.launch the record.
fn unwrap_launchers(bin: &Path) -> io::Result<()> {
   if !bin.is_dir() {
      return Ok(());
   }
   let records: Vec<Entry> = walk(bin)?
      .into_iter()
      .filter(|entry| {
         !entry.rel.contains('/') && entry.rel.starts_with('.') && entry.rel.ends_with(".launch")
      })
      .collect();
   for record in records {
      let name = &record.rel[1..record.rel.len() - ".launch".len()];
      remove_if_present(&bin.join(name))?;
      remove_file(&record.path)?;
      rename(bin.join(format!(".{name}")), bin.join(name))?;
   }
   Ok(())
}

/// `$ORIGIN/x` or a binary's `$ORIGIN/../lib/x` that lands inside the tree
/// already.
fn resolves_inside(name: &str, directory: &Path, out: &Path) -> bool {
   let Some(rest) = name.strip_prefix("$ORIGIN") else {
      return false;
   };
   normalize(&directory.join(rest.trim_start_matches('/'))).starts_with(out)
}

fn names_store_path(dir: &str) -> bool {
   dir.match_indices('-').any(|(index, _)| {
      index >= 32
         && dir.as_bytes()[index - 32..index]
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
   })
}

fn basename(path: &str) -> &str {
   path.rsplit('/').next().unwrap_or(path)
}
