use std::{
   env::var,
   fs::{
      DirEntry,
      File,
      create_dir_all,
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
   hash::Digest,
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

#[inline]
pub fn note(kind: &str, message: &str) {
   #[derive(Serialize)]
   #[serde(tag = "action", rename_all = "camelCase")]
   enum Event<'message> {
      SetPhase { phase: &'message str },
      Msg { level: u8, msg: String },
   }

   let event = if kind == "phase" {
      Event::SetPhase { phase: message }
   } else {
      Event::Msg {
         level: 3,
         msg:   format!("{kind}: {message}"),
      }
   };
   let mut stderr = io::stderr().lock();
   drop(write!(stderr, "@nix ").and_then(|()| {
      serde_json::to_writer(&mut stderr, &event)?;
      writeln!(stderr)
   }));
}

fn children(directory: &Path) -> io::Result<Vec<DirEntry>> {
   let mut entries: Vec<_> = read_dir(directory)?.collect::<io::Result<_>>()?;
   entries.sort_by_key(DirEntry::file_name);
   Ok(entries)
}

#[inline]
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

#[inline]
pub fn writable(path: &Path) -> io::Result<()> {
   let mode = metadata(path)?.permissions().mode();
   if mode & 0o200 == 0 {
      set_permissions(path, PermissionsExt::from_mode(mode | 0o200))?;
   }
   Ok(())
}

#[inline]
pub fn shebangs(directory: &Path, undo: bool) -> io::Result<()> {
   let ledger = PathBuf::from(
      var("NIX_BUILD_TOP").map_err(|_unset| io::Error::other("NIX_BUILD_TOP is not set"))?,
   )
   .join("shebangs");

   if undo && !ledger.exists() {
      return Ok(());
   }

   let candidates: Vec<PathBuf> = walk(directory)?
      .into_iter()
      .filter(|entry| entry.kind == 'f' && entry.size < SMALL_FILE)
      .map(|entry| PathBuf::from(entry.path))
      .filter(|path| {
         undo || metadata(path).is_ok_and(|meta| meta.permissions().mode() & 0o100 != 0)
      })
      .collect();

   if !undo {
      create_dir_all(&ledger)?;
   }

   parallel(&candidates, |path| fix(path, &ledger, undo))?;
   Ok(())
}

#[inline]
pub fn parallel<Item, Out, Work>(items: &[Item], work: Work) -> io::Result<Vec<Out>>
where
   Item: Sync,
   Out: Send,
   Work: Fn(&Item) -> io::Result<Out> + Sync,
{
   parallel_with(
      items,
      available_parallelism().map_or(4, |count| count.get().min(16)),
      work,
   )
}

#[inline]
pub fn parallel_with<Item, Out, Work>(
   items: &[Item],
   workers: usize,
   work: Work,
) -> io::Result<Vec<Out>>
where
   Item: Sync,
   Out: Send,
   Work: Fn(&Item) -> io::Result<Out> + Sync,
{
   let chunk = items.len().div_ceil(workers.max(1)).max(1);
   scope(|threads| {
      let handles: Vec<_> = items
         .chunks(chunk)
         .map(|batch| threads.spawn(|| batch.iter().map(&work).collect::<io::Result<Vec<Out>>>()))
         .collect();
      let mut results = Vec::with_capacity(items.len());
      for handle in handles {
         results.extend(
            handle
               .join()
               .map_err(|_panic| io::Error::other("tree worker panicked"))??,
         );
      }
      Ok(results)
   })
}

/// `ruby -x` stubs carry the real #! after a /bin/sh preamble, so env
/// shebangs also match after the first line.
fn fix(path: &Path, ledger: &Path, undo: bool) -> io::Result<()> {
   let bytes = read(path)?;
   let record = |contents: &[u8]| ledger.join(Digest::sha256(contents).hex());

   let fixed = if undo {
      let original = record(&bytes);

      match read(original) {
         Ok(contents) => contents,
         Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
         Err(error) => return Err(error),
      }
   } else {
      let mut rewritten = Vec::with_capacity(bytes.len());

      for (index, line) in bytes.split_inclusive(|byte| *byte == b'\n').enumerate() {
         match rewrite_shebang(line, index == 0) {
            Some(fixed_line) => rewritten.extend(fixed_line),
            None => rewritten.extend_from_slice(line),
         }
      }

      rewritten
   };

   if fixed == bytes {
      return Ok(());
   }

   if !undo {
      write_file(&record(&fixed), &bytes, None)?;
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

#[must_use]
#[inline]
pub fn replace_all(text: &[u8], from: &[u8], to: &[u8]) -> Vec<u8> {
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

fn rewrite_shebang(line: &[u8], first: bool) -> Option<Vec<u8>> {
   let text = str::from_utf8(line).ok()?.strip_prefix("#!")?;
   let interpreter = text.strip_prefix(' ').unwrap_or(text);
   let (program, args) = if let Some(env) = interpreter.strip_prefix("/usr/bin/env ") {
      let end = env.find(char::is_whitespace).unwrap_or(env.len());
      (&env[..end], &env[end..])
   } else {
      if !first {
         return None;
      }

      let absolute = ["/bin/", "/usr/bin/", "/usr/local/bin/"]
         .iter()
         .find_map(|prefix| interpreter.strip_prefix(prefix))?;

      if ["sh", "env"].iter().any(|excluded| {
         absolute.strip_prefix(excluded).is_some_and(|tail| {
            !tail
               .bytes()
               .next()
               .is_some_and(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
         })
      }) || !absolute
         .bytes()
         .next()
         .is_some_and(|byte| byte.is_ascii_lowercase())
      {
         return None;
      }

      let end = absolute
         .find(|ch: char| !ch.is_ascii_lowercase() && !ch.is_ascii_digit() && ch != '.')
         .unwrap_or(absolute.len());
      (&absolute[..end], &absolute[end..])
   };

   if program.is_empty() {
      return None;
   }

   let found = on_path(program);

   if !found.is_absolute() || !found.is_file() {
      return None;
   }

   Some(format!("#!{}{args}", found.display()).into_bytes())
}

#[must_use]
#[inline]
pub fn find(text: &[u8], needle: &[u8]) -> Option<usize> {
   if needle.is_empty() || text.len() < needle.len() {
      return None;
   }
   text
      .windows(needle.len())
      .position(|window| window == needle)
}

#[inline]
pub fn prune(root: &Path) -> io::Result<Vec<Entry>> {
   for docs in DOCS {
      match remove_dir_all(root.join(docs)) {
         Ok(()) => {},
         Err(error) if error.kind() == io::ErrorKind::NotFound => {},
         Err(error) => return Err(error),
      }
   }
   let (bundles, mut inventory): (Vec<Entry>, Vec<Entry>) = walk(root)?
      .into_iter()
      .partition(|entry| entry.kind == 'd' && extension_is(&entry.rel, "dSYM"));

   for bundle in &bundles {
      match remove_dir_all(&bundle.path) {
         Ok(()) => {},
         Err(error) if error.kind() == io::ErrorKind::NotFound => {},
         Err(error) => return Err(error),
      }
   }

   inventory.retain(|entry| {
      !bundles
         .iter()
         .any(|bundle| Path::new(&entry.rel).starts_with(&bundle.rel))
   });
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
   Ok(inventory)
}

#[inline]
pub fn print_pruned(root: &Path) -> io::Result<()> {
   let inventory = prune(root)?;
   let mut stdout = io::stdout().lock();
   serde_json::to_writer(&mut stdout, &inventory)?;
   writeln!(stdout)?;
   Ok(())
}

#[inline]
pub fn relativize(prefix: &str, dest: &str) -> io::Result<()> {
   let root = Path::new(prefix);
   let inventory = walk(root)?;
   let mut man_pages = 0_usize;

   for entry in inventory.iter().filter(|entry| entry.kind == 'f') {
      let variable = if extension_is(&entry.rel, "pc") {
         Some("pcfiledir")
      } else if extension_is(&entry.rel, "cmake") {
         Some("CMAKE_CURRENT_LIST_DIR")
      } else if entry.rel.starts_with("share/man/") {
         None
      } else {
         continue;
      };

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
      let relative = variable.map_or_else(|| "/usr".to_owned(), |name| format!("${{{name}}}/{up}"));
      let fixed = replace_all(&bytes, prefix.as_bytes(), relative.as_bytes());
      write_file(Path::new(&entry.path), &fixed, None)?;

      if variable.is_none() {
         man_pages += 1;
      }
   }

   if man_pages != 0 {
      note(
         "man",
         &format!("{man_pages} pages named the prefix, now /usr"),
      );
   }

   scripts(root, prefix)?;
   links(root, prefix, dest)?;
   leaks(&inventory, prefix)
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
