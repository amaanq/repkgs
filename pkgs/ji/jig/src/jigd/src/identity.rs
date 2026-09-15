use std::{
   collections::HashMap,
   fs::{
      File,
      canonicalize,
   },
   io,
   os::unix::fs::MetadataExt as _,
   path::{
      Component,
      Path,
      PathBuf,
   },
   sync::RwLock,
};
#[cfg(target_os = "linux")]
use std::{
   fs::read_link,
   os::fd::AsRawFd as _,
};

use wire::identity::Identity;

#[derive(Eq, PartialEq)]
struct Stamp {
   device:   u64,
   inode:    u64,
   size:     u64,
   modified: (i64, i64),
   changed:  (i64, i64),
}

struct Entry {
   stamp:    Stamp,
   identity: Identity,
}

pub struct Identities {
   store:      PathBuf,
   real_store: PathBuf,
   known:      RwLock<HashMap<String, Entry>>,
}

impl Identities {
   pub fn new(store: &Path) -> io::Result<Self> {
      Ok(Self {
         store:      store.to_path_buf(),
         real_store: canonicalize(store)?,
         known:      RwLock::new(HashMap::new()),
      })
   }

   pub fn of(&self, path: &str) -> io::Result<Identity> {
      let requested = Path::new(path);
      if !requested.starts_with(&self.store)
         || requested
            .components()
            .any(|part| matches!(part, Component::ParentDir | Component::CurDir))
      {
         return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "identity path is outside the store",
         ));
      }
      let real = canonicalize(requested)?;
      if !real.starts_with(&self.real_store) {
         return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "identity symlink leaves the store",
         ));
      }
      let mut file = File::open(real)?;
      #[cfg(target_os = "linux")]
      if !read_link(format!("/proc/self/fd/{}", file.as_raw_fd()))?.starts_with(&self.real_store) {
         return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "opened identity file is outside the store",
         ));
      }
      let metadata = file.metadata()?;
      if !metadata.is_file() {
         return Err(io::Error::other("identity requires a regular file"));
      }

      let stamp = Stamp {
         device:   metadata.dev(),
         inode:    metadata.ino(),
         size:     metadata.len(),
         modified: (metadata.mtime(), metadata.mtime_nsec()),
         changed:  (metadata.ctime(), metadata.ctime_nsec()),
      };
      {
         let known = self
            .known
            .read()
            .map_err(|_error| io::Error::other("identity lock poisoned"))?;
         if let Some(entry) = known.get(path)
            && entry.stamp == stamp
         {
            return Ok(entry.identity.clone());
         }
      }
      let identity = Identity::of_reader(&mut file)?;
      self
         .known
         .write()
         .map_err(|_error| io::Error::other("identity lock poisoned"))?
         .insert(path.to_owned(), Entry {
            stamp,
            identity: identity.clone(),
         });
      Ok(identity)
   }

   #[expect(
      clippy::expect_used,
      reason = "poisoned identity cache violates its invariants"
   )]
   pub fn len(&self) -> usize {
      self.known.read().expect("identity lock poisoned").len()
   }
}

#[cfg(test)]
#[expect(clippy::expect_used, reason = "tests")]
mod tests {
   use std::fs::write;

   use crate::{
      identity::Identities,
      test_directory::TestDirectory,
   };

   #[test]
   fn identity_matches_jig_and_tracks_rewrites() {
      let directory = TestDirectory::new();
      let path = directory.join("header.h");
      let filename = path.to_str().expect("UTF-8 test path");
      write(&path, b"abc").expect("write header");
      let identities = Identities::new(&directory).expect("identity store");
      let expected = "C:6437b3ac38465133ffb63b75273a8db5";
      assert_eq!(
         identities.of(filename).expect("hash header").to_string(),
         expected
      );
      assert_eq!(
         identities.of(filename).expect("reuse identity").to_string(),
         expected
      );
      assert_eq!(identities.len(), 1);
      write(&path, b"abcd").expect("rewrite header");
      assert_ne!(
         identities
            .of(filename)
            .expect("hash changed header")
            .to_string(),
         expected
      );
      assert!(
         identities
            .of(directory.join("missing").to_str().expect("UTF-8 path"))
            .is_err(),
         "missing file has no identity"
      );
      assert!(
         identities
            .of(directory.to_str().expect("UTF-8 path"))
            .is_err(),
         "directory has no identity"
      );
   }
}
