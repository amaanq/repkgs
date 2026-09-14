use std::{
   collections::HashMap,
   fs::File,
   io,
   os::unix::fs::MetadataExt as _,
   sync::RwLock,
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

#[derive(Default)]
pub struct Identities {
   known: RwLock<HashMap<String, Entry>>,
}

impl Identities {
   pub fn of(&self, path: &str) -> io::Result<Identity> {
      let mut file = File::open(path)?;
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
      let identities = Identities::default();
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
