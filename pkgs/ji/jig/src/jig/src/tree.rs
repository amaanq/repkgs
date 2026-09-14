use std::{
   env::var,
   fs::{
      DirEntry,
      File,
      metadata,
      read,
      read_dir,
      read_link,
      remove_dir_all,
      remove_file,
      set_permissions,
      symlink_metadata,
   },
   io::{
      self,
      Write as _,
   },
   os::unix::fs::{
      PermissionsExt,
      symlink,
   },
   path::{
      Path,
      PathBuf,
   },
   process::Command,
   thread::{
      available_parallelism,
      scope,
   },
};

use serde::Serialize;

use crate::{
   elf::relative_from,
   files::write_file,
   store::{
      STORE_DIR,
      normalize,
      on_path,
   },
};

const SMALL_FILE: u64 = 1024 * 1024;
const DOCS: [&str; 3] = ["share/doc", "share/info", "share/gtk-doc"];
const FROM_ZERO: &str = r#"prefix=$(cd "$(dirname "$0")/.." && pwd -P)"#;

#[derive(Serialize)]
pub struct Entry {
   #[serde(rename = "type")]
   pub kind:   char,
   pub size:   u64,
   pub path:   String,
   pub target: String,
   pub rel:    String,
}

pub fn note(kind: &str, message: &str) {
   let event =
      serde_json::json!({ "action": "msg", "level": 3_u8, "msg": format!("{kind}: {message}") });
   drop(writeln!(io::stderr().lock(), "@nix {event}"));
}

fn children(directory: &Path) -> io::Result<Vec<DirEntry>> {
   let mut entries: Vec<_> = read_dir(directory)?.collect::<io::Result<_>>()?;
   entries.sort_by_key(DirEntry::file_name);
   Ok(entries)
}

pub fn walk(root: &Path) -> io::Result<Vec<Entry>> {
   let mut found = Vec::new();
   let mut pending = vec![root.to_path_buf()];
   while let Some(directory) = pending.pop() {
      for child in children(&directory)? {
         let path = child.path();
         let meta = symlink_metadata(&path)?;
         let kind = if meta.is_symlink() {
            'l'
         } else if meta.is_dir() {
            pending.push(path.clone());
            'd'
         } else {
            'f'
         };
         let rel = path
            .strip_prefix(root)
            .map_err(io::Error::other)?
            .to_string_lossy()
            .into_owned();
         found.push(Entry {
            kind,
            size: meta.len(),
            path: path.to_string_lossy().into_owned(),
            target: if kind == 'l' {
               read_link(&path)?.to_string_lossy().into_owned()
            } else {
               String::new()
            },
            rel,
         });
      }
   }
   found.sort_by(|left, right| left.rel.cmp(&right.rel));
   Ok(found)
}

fn extension_is(rel: &str, extension: &str) -> bool {
   Path::new(rel)
      .extension()
      .is_some_and(|found| found == extension)
}

fn precompiled(rel: &str) -> bool {
   extension_is(rel, "pch") || extension_is(rel, "gch")
}

pub fn writable(path: &Path) -> io::Result<()> {
   let mode = metadata(path)?.permissions().mode();
   if mode & 0o200 == 0 {
      set_permissions(path, PermissionsExt::from_mode(mode | 0o200))?;
   }
   Ok(())
}

pub fn shebangs(directory: &Path, undo: bool) -> io::Result<()> {
   let env = on_path("env");
   if !env.is_absolute() {
      return Err(io::Error::other("env not on PATH"));
   }
   let ours = format!("#!{}", env.display());
   let (from, to): (&[&[u8]], &[u8]) = if undo {
      (&[ours.as_bytes()], b"#!/usr/bin/env")
   } else {
      (&[b"#!/usr/bin/env", b"#! /usr/bin/env"], ours.as_bytes())
   };
   let candidates: Vec<PathBuf> = walk(directory)?
      .into_iter()
      .filter(|entry| entry.kind == 'f' && entry.size < SMALL_FILE)
      .map(|entry| PathBuf::from(entry.path))
      .filter(|path| {
         undo || metadata(path).is_ok_and(|meta| meta.permissions().mode() & 0o100 != 0)
      })
      .collect();
   let workers = available_parallelism().map_or(4, |count| count.get().min(16));
   let chunk = candidates.len().div_ceil(workers).max(1);
   scope(|threads| {
      let mut handles = Vec::new();
      for paths in candidates.chunks(chunk) {
         handles.push(
            threads.spawn(move || paths.iter().try_for_each(|path| fix(path, from, to, undo))),
         );
      }
      for handle in handles {
         handle
            .join()
            .map_err(|_panic| io::Error::other("shebang worker panicked"))??;
      }
      Ok(())
   })
}

/// `ruby -x` stubs carry the real #! after a /bin/sh preamble, so every
/// line start counts, both ways.
fn fix(path: &Path, from: &[&[u8]], to: &[u8], undo: bool) -> io::Result<()> {
   let bytes = read(path)?;
   let fixed = replace_line_starts(&bytes, from, to);
   if fixed == bytes {
      return Ok(());
   }
   let meta = metadata(path)?;
   writable(path)?;
   let mut file = File::create(path)?;
   file.write_all(&fixed)?;
   if !undo {
      file.set_modified(meta.modified()?)?;
   }
   Ok(())
}

fn replace_all(text: &[u8], from: &[u8], to: &[u8]) -> Vec<u8> {
   let mut output = Vec::with_capacity(text.len());
   let mut rest = text;
   while let Some(position) = find(rest, from) {
      output.extend_from_slice(&rest[..position]);
      output.extend_from_slice(to);
      rest = &rest[position + from.len()..];
   }
   output.extend_from_slice(rest);
   output
}

fn replace_line_starts(text: &[u8], from: &[&[u8]], to: &[u8]) -> Vec<u8> {
   let mut output = Vec::with_capacity(text.len());
   let mut at_line_start = true;
   let mut rest = text;
   while !rest.is_empty() {
      if at_line_start && let Some(matched) = from.iter().find(|needle| rest.starts_with(needle)) {
         output.extend_from_slice(to);
         rest = &rest[matched.len()..];
         at_line_start = false;
         continue;
      }
      let end = rest
         .iter()
         .position(|byte| *byte == b'\n')
         .map_or(rest.len(), |nl| nl + 1);
      output.extend_from_slice(&rest[..end]);
      at_line_start = rest[end - 1] == b'\n';
      rest = &rest[end..];
   }
   output
}

fn find(text: &[u8], needle: &[u8]) -> Option<usize> {
   if needle.is_empty() || text.len() < needle.len() {
      return None;
   }
   text
      .windows(needle.len())
      .position(|window| window == needle)
}

pub fn prune(root: &Path) -> io::Result<()> {
   for docs in DOCS {
      match remove_dir_all(root.join(docs)) {
         Ok(()) => {},
         Err(error) if error.kind() == io::ErrorKind::NotFound => {},
         Err(error) => return Err(error),
      }
   }
   let mut inventory = walk(root)?;
   let mut junk = Vec::new();
   let mut compressed = Vec::new();
   for entry in inventory.iter().filter(|entry| entry.kind == 'f') {
      let rel = entry.rel.as_str();
      if extension_is(rel, "la")
         || rel.ends_with("/perllocal.pod")
         || rel.ends_with("/.packlist")
         || rel == "lib/charset.alias"
      {
         junk.push(entry.rel.clone());
      } else if rel.starts_with("share/man/") && extension_is(rel, "gz") {
         compressed.push(entry.path.clone());
      } else if precompiled(rel) {
         let shown: Vec<_> = inventory
            .iter()
            .filter(|other| precompiled(&other.rel))
            .take(3)
            .map(|other| other.rel.as_str())
            .collect();
         return Err(io::Error::other(format!(
            "precompiled headers in output do not relocate: {}",
            shown.join(" ")
         )));
      }
   }
   for rel in &junk {
      remove_file(root.join(rel))?;
   }
   if !compressed.is_empty() {
      drop(writeln!(
         io::stderr().lock(),
         "+ gzip -d {}",
         compressed.join(" ")
      ));
      let status = Command::new("gzip").arg("-d").args(&compressed).status()?;
      if !status.success() {
         return Err(io::Error::other("gzip -d failed"));
      }
   }
   inventory.retain(|entry| !junk.contains(&entry.rel));
   for entry in &mut inventory {
      if compressed.contains(&entry.path) {
         entry.rel.truncate(entry.rel.len() - 3);
         entry.path = format!("{}/{}", root.display(), entry.rel);
      }
   }
   let mut stdout = io::stdout().lock();
   serde_json::to_writer(&mut stdout, &inventory)?;
   writeln!(stdout)?;
   Ok(())
}

pub fn relativize(prefix: &str, dest: &str, macho: bool) -> io::Result<()> {
   let root = Path::new(prefix);
   let inventory = walk(root)?;
   if macho {
      sonames(&inventory, prefix)?;
   }
   for entry in inventory
      .iter()
      .filter(|entry| entry.kind == 'f' && extension_is(&entry.rel, "pc"))
   {
      let bytes = read(&entry.path)?;
      if find(&bytes, prefix.as_bytes()).is_none() {
         continue;
      }
      let up = Path::new(&entry.rel)
         .parent()
         .map(|parent| {
            parent
               .components()
               .map(|_| "..")
               .collect::<Vec<_>>()
               .join("/")
         })
         .unwrap_or_default();
      let fixed = replace_all(
         &bytes,
         prefix.as_bytes(),
         format!("${{pcfiledir}}/{up}").as_bytes(),
      );
      write_file(Path::new(&entry.path), &fixed, None)?;
   }
   scripts(root, prefix)?;
   links(root, prefix, dest)?;
   leaks(&inventory, prefix)
}

/// reloc-fixup made every `LC_ID_DYLIB` `@rpath/<name>`, cmake's exported
/// targets still record the `install_name` the project chose (taglib sets
/// `INSTALL_NAME_DIR` to its libdir).
fn sonames(inventory: &[Entry], prefix: &str) -> io::Result<()> {
   for entry in inventory
      .iter()
      .filter(|entry| entry.kind == 'f' && extension_is(&entry.rel, "cmake"))
   {
      let text = String::from_utf8_lossy(&read(&entry.path)?).into_owned();
      if !text.contains("IMPORTED_SONAME_") {
         continue;
      }
      let fixed: Vec<String> = text
         .lines()
         .map(|line| soname_line(line, prefix).unwrap_or_else(|| line.to_owned()))
         .collect();
      let mut joined = fixed.join("\n");
      if text.ends_with('\n') {
         joined.push('\n');
      }
      if joined != text {
         write_file(Path::new(&entry.path), joined.as_bytes(), None)?;
      }
   }
   Ok(())
}

fn soname_line(line: &str, prefix: &str) -> Option<String> {
   let start = line.find("IMPORTED_SONAME_")?;
   let rest = &line[start..];
   let space = rest.find(' ')?;
   let key = &rest[..space];
   let path = rest[space..]
      .strip_prefix(" \"")?
      .strip_prefix(prefix)?
      .strip_prefix('/')?;
   let end = path.find('"')?;
   let name = path[..end].rsplit('/').next()?;
   Some(format!(
      "{}{key} \"@rpath/{name}\"{}",
      &line[..start],
      &path[end + 1..]
   ))
}

fn scripts(root: &Path, prefix: &str) -> io::Result<()> {
   let bin = root.join("bin");
   if !bin.is_dir() {
      return Ok(());
   }
   for child in children(&bin)? {
      let path = child.path();
      if !symlink_metadata(&path)?.is_file() {
         continue;
      }
      let bytes = read(&path)?;
      if find(&bytes, prefix.as_bytes()).is_none() {
         continue;
      }
      let Ok(script) = String::from_utf8(bytes) else {
         continue;
      };
      let mut lines: Vec<&str> = script.split('\n').collect();
      if lines.last() == Some(&"") {
         lines.pop();
      }
      let assignment = format!("prefix={prefix}");
      let variable = format!("${{{}}}", "prefix");
      let assigns = |line: &str| line.replace(['"', '\''], "") == assignment;
      let shell = lines
         .first()
         .is_some_and(|first| first.starts_with("#!") && first.ends_with("sh"));
      if !shell || !lines.iter().any(|line| assigns(line)) {
         continue;
      }
      let fixed: Vec<String> = lines
         .iter()
         .map(|line| {
            if assigns(line) {
               FROM_ZERO.to_owned()
            } else {
               line.replace(prefix, &variable)
            }
         })
         .collect();
      write_file(&path, fixed.join("\n").as_bytes(), None)?;
      note(
         "script",
         &format!(
            "bin/{}: prefix from $0",
            child.file_name().to_string_lossy()
         ),
      );
   }
   Ok(())
}

fn links(root: &Path, prefix: &str, dest: &str) -> io::Result<()> {
   let store = format!(
      "{}/",
      var("NIX_STORE").unwrap_or_else(|_| STORE_DIR.to_owned())
   );
   let under_prefix = format!("{prefix}/");
   let under_dest = format!("{dest}/");
   for entry in walk(root)?.iter().filter(|entry| entry.kind == 'l') {
      let placed = format!("{dest}/{}", entry.rel);
      let target = &entry.target;
      let final_target = target.strip_prefix(&under_prefix).map_or_else(
         || {
            let parent = Path::new(&placed)
               .parent()
               .unwrap_or_else(|| Path::new("/"));
            normalize(&parent.join(target))
               .to_string_lossy()
               .into_owned()
         },
         |inside| format!("{dest}/{inside}"),
      );
      if !final_target.starts_with(&store) {
         return Err(io::Error::other(format!(
            "symlink {} -> {target} leaves the store",
            entry.rel
         )));
      }
      if target.starts_with('/') {
         let from = Path::new(&placed)
            .parent()
            .unwrap_or_else(|| Path::new("/"));
         let relative = relative_from(from, Path::new(&final_target));
         remove_file(&entry.path)?;
         symlink(&relative, &entry.path)?;
      }
      let here = final_target.strip_prefix(&under_dest).map_or_else(
         || final_target.clone(),
         |inside| format!("{prefix}/{inside}"),
      );
      if symlink_metadata(&here).is_err() {
         return Err(io::Error::other(format!(
            "symlink {} -> {target} dangles",
            entry.rel
         )));
      }
   }
   Ok(())
}

fn leaks(inventory: &[Entry], prefix: &str) -> io::Result<()> {
   let needle = prefix.as_bytes();
   let mut hits = Vec::new();
   for entry in inventory.iter().filter(|entry| entry.kind == 'f') {
      let bytes = read(&entry.path)?;
      if find(&bytes, needle).is_some() {
         hits.push((entry.rel.clone(), bytes));
      }
   }
   if hits.is_empty() {
      return Ok(());
   }
   let mut detail = Vec::new();
   for hit in hits.iter().take(10) {
      let (rel, bytes) = (&hit.0, &hit.1);
      let mut shown: Vec<String> = Vec::new();
      for run in bytes
         .split(|byte| !(byte.is_ascii_graphic() || *byte == b' ' || *byte == b'\t'))
         .filter(|run| run.len() >= needle.len() && find(run, needle).is_some())
      {
         let masked: String = String::from_utf8_lossy(run)
            .replace(prefix, "$out")
            .chars()
            .take(120)
            .collect();
         if !shown.contains(&masked) {
            shown.push(masked);
         }
         if shown.len() == 3 {
            break;
         }
      }
      detail.push(format!("  {rel}: {}", shown.join(", ")));
   }
   let more = if hits.len() > 10 {
      format!("\n  ... and {} more", hits.len() - 10)
   } else {
      String::new()
   };
   Err(io::Error::other(format!(
      "not relocatable: {} files name the install prefix (shown as $out). Make the path relative \
       to the file (reloc.h RELOC, ${{pcfiledir}}, $ORIGIN) or configure it away:\n{}{more}",
      hits.len(),
      detail.join("\n")
   )))
}
