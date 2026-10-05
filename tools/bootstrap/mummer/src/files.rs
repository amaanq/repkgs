use std::{
   fs::{
      self,
      Permissions,
      read_dir,
      symlink_metadata,
   },
   io::{
      self,
      Write as _,
   },
   os::unix::fs::PermissionsExt as _,
   path::{
      Component,
      Path,
      PathBuf,
   },
};

use tempfile::NamedTempFile;

#[inline]
pub fn write_file(path: &Path, bytes: &[u8], mode: Option<u32>) -> io::Result<()> {
   let parent = path
      .parent()
      .filter(|parent| !parent.as_os_str().is_empty())
      .unwrap_or_else(|| Path::new("."));
   let mut temporary = NamedTempFile::new_in(parent)?;
   temporary.write_all(bytes)?;
   let permissions = match mode {
      Some(bits) => Permissions::from_mode(bits),
      None => {
         match fs::metadata(path) {
            Ok(metadata) => metadata.permissions(),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Permissions::from_mode(0o644),
            Err(error) => return Err(error),
         }
      },
   };
   temporary.as_file().set_permissions(permissions)?;
   temporary.persist(path).map_err(|error| error.error)?;
   Ok(())
}

/// A path as text, an error naming it when it is not UTF-8.
#[inline]
pub fn utf8(path: &Path) -> io::Result<&str> {
   path
      .to_str()
      .ok_or_else(|| io::Error::other(format!("{} is not UTF-8", path.display())))
}

/// A directory's entries in name order.
#[inline]
pub fn sorted_entries(directory: &Path) -> io::Result<Vec<PathBuf>> {
   let mut found = read_dir(directory)?
      .map(|entry| entry.map(|found| found.path()))
      .collect::<io::Result<Vec<_>>>()?;
   found.sort();
   Ok(found)
}

#[inline]
pub fn remove_if_exists(path: &Path) -> io::Result<()> {
   match fs::remove_file(path) {
      Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
      Ok(()) | Err(_) => Ok(()),
   }
}

#[inline]
pub fn relative_path(text: &str) -> io::Result<&Path> {
   let path = Path::new(text);
   if text.is_empty()
      || text.contains(['\n', '\r', '\t', '\0'])
      || path
         .components()
         .any(|part| !matches!(part, Component::Normal(_)))
   {
      return Err(io::Error::other("invalid relative cache path"));
   }
   Ok(path)
}

#[inline]
pub fn glob(pattern: &str) -> io::Result<Vec<PathBuf>> {
   let mut found = Vec::new();
   for expanded in expand_braces(pattern) {
      let (mut candidates, segments) = expanded.strip_prefix('/').map_or_else(
         || {
            (
               vec![PathBuf::new()],
               expanded.split('/').collect::<Vec<_>>(),
            )
         },
         |rest| {
            (
               vec![PathBuf::from("/")],
               rest.split('/').collect::<Vec<_>>(),
            )
         },
      );
      for segment in segments.into_iter().filter(|segment| !segment.is_empty()) {
         let mut next = Vec::new();
         for candidate in &candidates {
            if segment == "**" {
               next.push(candidate.clone());
               next.extend(descendants(candidate)?);
            } else if segment.contains('*') {
               let directory = if candidate.as_os_str().is_empty() {
                  PathBuf::from(".")
               } else {
                  candidate.clone()
               };
               let Ok(entries) = read_dir(&directory) else {
                  continue;
               };
               for entry in entries {
                  let name = entry?.file_name();
                  let text = name.to_string_lossy();
                  if !text.starts_with('.') && wildcard(segment, &text) {
                     next.push(candidate.join(&name));
                  }
               }
            } else {
               let joined = candidate.join(segment);
               if symlink_metadata(&joined).is_ok() {
                  next.push(joined);
               }
            }
         }
         candidates = next;
      }
      found.extend(candidates);
   }
   found.sort();
   found.dedup();
   Ok(found)
}

fn expand_braces(pattern: &str) -> Vec<String> {
   let Some(open) = pattern.find('{') else {
      return vec![pattern.to_owned()];
   };
   let Some(close) = pattern[open..].find('}').map(|offset| open + offset) else {
      return vec![pattern.to_owned()];
   };
   let (head, tail) = (&pattern[..open], &pattern[close + 1..]);
   pattern[open + 1..close]
      .split(',')
      .flat_map(|choice| expand_braces(&format!("{head}{choice}{tail}")))
      .collect()
}

fn wildcard(pattern: &str, text: &str) -> bool {
   let Some((head, rest)) = pattern.split_once('*') else {
      return pattern == text;
   };
   let Some(after) = text.strip_prefix(head) else {
      return false;
   };
   if rest.is_empty() {
      return true;
   }
   (0..=after.len())
      .filter(|&skip| after.is_char_boundary(skip))
      .any(|skip| wildcard(rest, &after[skip..]))
}

fn descendants(directory: &Path) -> io::Result<Vec<PathBuf>> {
   let listed = if directory.as_os_str().is_empty() {
      PathBuf::from(".")
   } else {
      directory.to_path_buf()
   };
   let Ok(entries) = read_dir(&listed) else {
      return Ok(Vec::new());
   };
   let mut found = Vec::new();
   for entry in entries {
      let name = entry?.file_name();
      if name.to_string_lossy().starts_with('.') {
         continue;
      }
      let child = directory.join(&name);
      if symlink_metadata(&child)?.is_dir() {
         found.extend(descendants(&child)?);
         found.push(child);
      }
   }
   Ok(found)
}
