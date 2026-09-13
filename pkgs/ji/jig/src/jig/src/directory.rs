use std::{
   collections::BTreeSet,
   fmt::Write,
   fs::{
      DirEntry,
      create_dir_all,
      read,
      read_dir,
      read_link,
      remove_dir,
      rename,
      symlink_metadata,
   },
   io,
   os::unix::fs::{
      PermissionsExt,
      symlink,
   },
   path::{
      Path,
      PathBuf,
   },
   slice::from_ref,
   string::FromUtf8Error,
};

use tempfile::Builder;
use wire::{
   client::Client,
   digest,
};

use crate::{
   files::{
      relative_path,
      write_file,
   },
   process::diagnostic,
   store::{
      Store,
      mask_hashes,
   },
};

fn as_text(data: Vec<u8>) -> Result<String, Vec<u8>> {
   if data.contains(&0) {
      return Err(data);
   }
   String::from_utf8(data).map_err(FromUtf8Error::into_bytes)
}

fn stored(data: Vec<u8>) -> Vec<u8> {
   match as_text(data) {
      Ok(text) => mask_hashes(&text).into_bytes(),
      Err(binary) => binary,
   }
}

fn restored(store: &Store, data: Vec<u8>) -> Vec<u8> {
   match as_text(data) {
      Ok(text) => store.resolve_all(&text).into_bytes(),
      Err(binary) => binary,
   }
}

struct Entry {
   path: PathBuf,
   kind: Kind,
}

struct Blob {
   mode: u32,
   hash: String,
}

enum Kind {
   File(Blob),
   Link(PathBuf),
   Directory,
}

pub fn put_directory(cache: &mut Client, key: &str, root: &Path) -> io::Result<()> {
   let mut listing = String::new();
   let mut pending = vec![root.to_path_buf()];
   let mut files = 0;
   let mut uploaded = 0;
   while let Some(directory) = pending.pop() {
      let mut children = read_dir(directory)?.collect::<io::Result<Vec<_>>>()?;
      children.sort_unstable_by_key(DirEntry::file_name);
      for child in children {
         let path = child.path();
         let relative = path.strip_prefix(root).map_err(io::Error::other)?;
         let name = relative
            .to_str()
            .ok_or_else(|| io::Error::other("cache path is not UTF-8"))?;
         relative_path(name)?;
         let metadata = symlink_metadata(&path)?;
         if metadata.is_symlink() {
            let target = read_link(&path)?;
            let target_text = target
               .to_str()
               .ok_or_else(|| io::Error::other("cache symlink target is not UTF-8"))?;
            if target_text.contains(['\n', '\r', '\t', '\0']) {
               return Err(io::Error::other(
                  "cache symlink target contains a line delimiter",
               ));
            }
            writeln!(listing, "l 0 {target_text}\t{name}")
               .expect("formatting a string cannot fail");
         } else if metadata.is_dir() {
            writeln!(listing, "d 0 -\t{name}").expect("formatting a string cannot fail");
            pending.push(path);
         } else if metadata.is_file() {
            let data = stored(read(&path)?);
            let hash = digest(&data);
            let blob_key = format!("b/{hash}");
            if !cache.has_many(from_ref(&blob_key))?[0] {
               cache.put(&blob_key, &data)?;
               uploaded += 1;
            }
            writeln!(
               listing,
               "f {:o} {hash}\t{name}",
               metadata.permissions().mode() & 0o777
            )
            .expect("formatting a string cannot fail");
            files += 1;
         } else {
            return Err(io::Error::other(format!(
               "unsupported cache file {}",
               path.display()
            )));
         }
      }
   }
   cache.put(key, listing.as_bytes())?;
   diagnostic(format_args!(
      "jig stored {key}, {files} files, {uploaded} new"
   ));
   Ok(())
}

fn parse_listing(text: &str) -> io::Result<Vec<Entry>> {
   let mut entries = Vec::new();
   let mut paths = BTreeSet::new();
   let mut leaves = BTreeSet::new();
   for line in text.lines() {
      let (head, name) = line
         .split_once('\t')
         .ok_or_else(|| io::Error::other("invalid cache directory listing"))?;
      let path = relative_path(name)?.to_path_buf();
      if !paths.insert(path.clone()) {
         return Err(io::Error::other("duplicate cache directory entry"));
      }
      let mut fields = head.splitn(3, ' ');
      let kind = fields.next();
      let mode = fields
         .next()
         .ok_or_else(|| io::Error::other("missing cache file mode"))?;
      let reference = fields
         .next()
         .ok_or_else(|| io::Error::other("missing cache file reference"))?;
      let kind = match kind {
         Some("f") => {
            let bits = u32::from_str_radix(mode, 8).map_err(io::Error::other)?;
            if bits > 0o777
               || reference.len() != 32
               || !reference.bytes().all(|byte| byte.is_ascii_hexdigit())
            {
               return Err(io::Error::other("invalid cached file metadata"));
            }
            leaves.insert(path.clone());
            Kind::File(Blob {
               mode: bits,
               hash: reference.to_owned(),
            })
         },
         Some("l") => {
            if reference.is_empty() || reference.contains('\0') {
               return Err(io::Error::other("invalid cached symlink target"));
            }
            leaves.insert(path.clone());
            Kind::Link(PathBuf::from(reference))
         },
         Some("d") => Kind::Directory,
         _ => return Err(io::Error::other("unknown cached file kind")),
      };
      entries.push(Entry { path, kind });
   }
   if entries.iter().any(|entry| {
      entry
         .path
         .ancestors()
         .skip(1)
         .any(|parent| leaves.contains(parent))
   }) {
      return Err(io::Error::other(
         "cached file appears beneath a file or symlink",
      ));
   }
   Ok(entries)
}

pub fn get_directory(cache: &mut Client, key: &str, root: &Path) -> io::Result<bool> {
   let Some(listing) = cache.get(key)? else {
      return Ok(false);
   };
   let text = String::from_utf8(listing).map_err(io::Error::other)?;
   let entries = parse_listing(&text)?;
   let parent = root
      .parent()
      .filter(|path| !path.as_os_str().is_empty())
      .unwrap_or_else(|| Path::new("."));
   create_dir_all(parent)?;
   let staging = Builder::new().prefix(".jig-dir-").tempdir_in(parent)?;
   let store = Store::new();
   let mut files = 0;
   for batch in entries.chunks(256) {
      let keys: Vec<_> = batch
         .iter()
         .filter_map(|entry| {
            if let Kind::File(blob) = &entry.kind {
               Some(format!("b/{}", blob.hash))
            } else {
               None
            }
         })
         .collect();
      let mut values = cache.get_many(&keys)?.into_iter();
      for entry in batch {
         let destination = staging.path().join(&entry.path);
         let containing = destination
            .parent()
            .ok_or_else(|| io::Error::other("cache entry has no parent"))?;
         create_dir_all(containing)?;
         match &entry.kind {
            Kind::File(blob) => {
               let Some(Some(data)) = values.next() else {
                  return Ok(false);
               };
               if digest(&data) != blob.hash {
                  return Err(io::Error::other("cached file content hash differs"));
               }
               write_file(&destination, &restored(&store, data), Some(blob.mode))?;
               files += 1;
            },
            Kind::Link(target) => symlink(target, destination)?,
            Kind::Directory => create_dir_all(destination)?,
         }
      }
   }
   match symlink_metadata(root) {
      Ok(metadata) if metadata.is_dir() => remove_dir(root)?,
      Ok(_) => return Err(io::Error::other("cache destination is not a directory")),
      Err(error) if error.kind() == io::ErrorKind::NotFound => {},
      Err(error) => return Err(error),
   }
   rename(staging.path(), root)?;
   diagnostic(format_args!("jig restored {key}, {files} files"));
   Ok(true)
}
