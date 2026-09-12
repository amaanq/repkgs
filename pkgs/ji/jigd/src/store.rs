use std::{
   collections::{
      BTreeMap,
      HashMap,
   },
   fmt::{
      Display,
      Formatter,
      Result as FormatResult,
   },
   fs::{
      File,
      OpenOptions,
      create_dir_all,
      read_dir,
      remove_file,
      rename,
   },
   io::{
      self,
      BufReader,
      BufWriter,
      Read,
      Write,
   },
   os::unix::{
      fs::FileExt,
      net::UnixStream,
   },
   path::{
      Path,
      PathBuf,
   },
   sync::{
      Arc,
      atomic::{
         AtomicU64,
         Ordering,
      },
   },
};

use rustix::{
   fs::{
      FlockOperation,
      flock,
      sendfile,
   },
   io::Errno,
};
use tracing::{
   info,
   warn,
};

const PACK_LIMIT: u64 = 256 << 20;
const HEADER_SIZE: u64 = 8;
pub const MAX_VALUE_SIZE: usize = 2 << 30;

struct Location {
   pack:   u64,
   offset: u64,
   length: u32,
}

struct Pack {
   file: Arc<File>,
   size: u64,
   live: u64,
   used: AtomicU64,
}

pub struct Value {
   file:   Arc<File>,
   offset: u64,
   length: u32,
}

impl Value {
   pub const fn size(&self) -> u32 {
      self.length
   }

   pub fn write_to(&self, stream: &UnixStream) -> io::Result<()> {
      let mut offset = self.offset;
      let mut remaining = self.length as usize;
      while remaining > 0 {
         match sendfile(
            stream,
            &*self.file,
            Some(&mut offset),
            remaining.min(1 << 20),
         ) {
            Ok(0) => {
               return Err(io::Error::new(
                  io::ErrorKind::UnexpectedEof,
                  "truncated cache object",
               ));
            },
            Ok(written) => remaining -= written,
            Err(Errno::INTR) => {},
            Err(error) => return Err(error.into()),
         }
      }
      Ok(())
   }
}

pub struct Stats {
   keys:  usize,
   packs: usize,
   bytes: u64,
   live:  u64,
}

impl Display for Stats {
   #[expect(
      clippy::renamed_function_params,
      reason = "Project names have at least two characters"
   )]
   fn fmt(&self, formatter: &mut Formatter<'_>) -> FormatResult {
      write!(
         formatter,
         "keys={} packs={} bytes={} live={}",
         self.keys, self.packs, self.bytes, self.live
      )
   }
}

pub struct Store {
   directory:       PathBuf,
   _directory_lock: File,
   budget:          u64,
   reads:           AtomicU64,
   index:           HashMap<String, Location>,
   packs:           BTreeMap<u64, Pack>,
   active:          u64,
   total:           u64,
}

impl Store {
   pub fn open(directory: &Path, budget: u64) -> io::Result<Self> {
      create_dir_all(directory)?;
      let directory_lock = File::open(directory)?;
      flock(&directory_lock, FlockOperation::NonBlockingLockExclusive)?;
      let mut store = Self {
         directory: directory.to_owned(),
         _directory_lock: directory_lock,
         budget,
         reads: AtomicU64::new(0),
         index: HashMap::new(),
         packs: BTreeMap::new(),
         active: 0,
         total: 0,
      };
      let mut identifiers = Vec::new();
      for entry in read_dir(directory)? {
         let path = entry?.path();
         if path
            .extension()
            .is_some_and(|extension| extension == "pack")
         {
            let number = path
               .file_stem()
               .and_then(|stem| stem.to_str())
               .ok_or_else(|| io::Error::other("invalid pack filename"))?;
            let identifier = number.parse::<u64>().map_err(io::Error::other)?;
            if path.file_name() != store.pack_path(identifier, "pack").file_name() {
               return Err(io::Error::other("noncanonical pack filename"));
            }
            identifiers.push(identifier);
         }
      }
      identifiers.sort_unstable();
      for identifier in identifiers {
         store.load(identifier)?;
         store.active = identifier;
      }
      store.rotate()?;
      store.evict()?;
      Ok(store)
   }

   fn pack_path(&self, identifier: u64, extension: &str) -> PathBuf {
      self.directory.join(format!("{identifier:06}.{extension}"))
   }

   fn insert(&mut self, key: String, location: Location) {
      self
         .packs
         .get_mut(&location.pack)
         .expect("missing new pack")
         .live += u64::from(location.length);
      if let Some(previous) = self.index.insert(key, location) {
         self
            .packs
            .get_mut(&previous.pack)
            .expect("missing previous pack")
            .live -= u64::from(previous.length);
      }
   }

   fn load(&mut self, identifier: u64) -> io::Result<()> {
      let file = Arc::new(
         OpenOptions::new()
            .read(true)
            .write(true)
            .open(self.pack_path(identifier, "pack"))?,
      );
      let size = file.metadata()?.len();
      self.packs.insert(identifier, Pack {
         file: Arc::clone(&file),
         size,
         live: 0,
         used: AtomicU64::new(0),
      });
      match File::open(self.pack_path(identifier, "hint")) {
         Ok(hint) => {
            let mut reader = BufReader::new(hint);
            while let Some(header) = read_header::<16>(&mut reader)? {
               let key_length =
                  u32::from_le_bytes(header[..4].try_into().expect("key length slice"));
               let length =
                  u32::from_le_bytes(header[4..8].try_into().expect("value length slice"));
               let record_offset =
                  u64::from_le_bytes(header[8..].try_into().expect("offset slice"));
               check_lengths(key_length, length)?;
               let offset = record_offset
                  .checked_add(HEADER_SIZE + u64::from(key_length))
                  .ok_or_else(|| io::Error::other("cache offset overflow"))?;
               if offset
                  .checked_add(u64::from(length))
                  .is_none_or(|end| end > size)
               {
                  return Err(io::Error::other("cache hint exceeds pack size"));
               }
               let key = read_key(&mut reader, key_length)?;
               self.insert(key, Location {
                  pack: identifier,
                  offset,
                  length,
               });
            }
         },
         Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let mut reader = BufReader::new(&*file);
            let mut offset = 0;
            while offset + HEADER_SIZE <= size {
               let mut header = [0; 8];
               reader.read_exact(&mut header)?;
               let key_length =
                  u32::from_le_bytes(header[..4].try_into().expect("key length slice"));
               let length = u32::from_le_bytes(header[4..].try_into().expect("value length slice"));
               check_lengths(key_length, length)?;
               let end = offset + HEADER_SIZE + u64::from(key_length) + u64::from(length);
               if end > size {
                  break;
               }
               let key = read_key(&mut reader, key_length)?;
               reader.seek_relative(i64::from(length))?;
               self.insert(key, Location {
                  pack: identifier,
                  offset: offset + HEADER_SIZE + u64::from(key_length),
                  length,
               });
               offset = end;
            }
            if offset != size {
               warn!("truncating unfinished pack {identifier} from {size} to {offset} bytes");
               file.set_len(offset)?;
               self
                  .packs
                  .get_mut(&identifier)
                  .expect("missing loaded pack")
                  .size = offset;
            }
            self.seal(identifier)?;
         },
         Err(error) => return Err(error),
      }
      self.total += self.packs[&identifier].size;
      Ok(())
   }

   fn seal(&self, identifier: u64) -> io::Result<()> {
      let temporary = self.pack_path(identifier, "hint.tmp");
      let mut writer = BufWriter::new(File::create(&temporary)?);
      for (key, location) in &self.index {
         if location.pack != identifier {
            continue;
         }
         let key_length = u32::try_from(key.len()).expect("validated key length");
         let record_offset = location.offset - HEADER_SIZE - u64::from(key_length);
         writer.write_all(&key_length.to_le_bytes())?;
         writer.write_all(&location.length.to_le_bytes())?;
         writer.write_all(&record_offset.to_le_bytes())?;
         writer.write_all(key.as_bytes())?;
      }
      writer.flush()?;
      rename(temporary, self.pack_path(identifier, "hint"))
   }

   fn rotate(&mut self) -> io::Result<()> {
      if self.packs.contains_key(&self.active) {
         self.seal(self.active)?;
      }
      let identifier = self
         .active
         .checked_add(1)
         .ok_or_else(|| io::Error::other("pack IDs exhausted"))?;
      let file = OpenOptions::new()
         .read(true)
         .write(true)
         .create_new(true)
         .open(self.pack_path(identifier, "pack"))?;
      self.packs.insert(identifier, Pack {
         file: Arc::new(file),
         size: 0,
         live: 0,
         used: AtomicU64::new(0),
      });
      self.active = identifier;
      Ok(())
   }

   pub fn get(&self, key: &str) -> Option<Value> {
      let location = self.index.get(key)?;
      let pack = &self.packs[&location.pack];
      pack.used.store(
         self.reads.fetch_add(1, Ordering::Relaxed) + 1,
         Ordering::Relaxed,
      );
      Some(Value {
         file:   Arc::clone(&pack.file),
         offset: location.offset,
         length: location.length,
      })
   }

   pub fn put(&mut self, key: &str, value: &[u8]) -> io::Result<()> {
      let key_length = u32::try_from(key.len()).map_err(io::Error::other)?;
      let length = u32::try_from(value.len()).map_err(io::Error::other)?;
      check_lengths(key_length, length)?;
      check_key(key)?;
      let record_size = HEADER_SIZE + u64::from(key_length) + u64::from(length);
      let active_size = self.packs[&self.active].size;
      if active_size > 0 && active_size + record_size > PACK_LIMIT {
         self.rotate()?;
      }
      let pack = self
         .packs
         .get_mut(&self.active)
         .expect("missing active pack");
      let mut header = Vec::with_capacity(8 + key.len());
      header.extend_from_slice(&key_length.to_le_bytes());
      header.extend_from_slice(&length.to_le_bytes());
      header.extend_from_slice(key.as_bytes());
      let offset = pack.size + header.len() as u64;
      let written = pack
         .file
         .write_all_at(&header, pack.size)
         .and_then(|()| pack.file.write_all_at(value, offset));
      if let Err(error) = written {
         pack.file.set_len(pack.size)?;
         return Err(error);
      }
      pack.size += record_size;
      self.total += record_size;
      self.insert(key.to_owned(), Location {
         pack: self.active,
         offset,
         length,
      });
      self.evict()
   }

   fn evict(&mut self) -> io::Result<()> {
      if self.budget == 0 {
         return Ok(());
      }
      let mut candidates: Vec<_> = self
         .packs
         .iter()
         .filter(|(identifier, _)| **identifier != self.active)
         .map(|(identifier, pack)| (pack.used.load(Ordering::Relaxed), *identifier))
         .collect();
      candidates.sort_unstable();
      for (_, identifier) in candidates {
         if self.total <= self.budget {
            break;
         }
         remove_file(self.pack_path(identifier, "pack"))?;
         remove_file(self.pack_path(identifier, "hint"))?;
         self.index.retain(|_, location| location.pack != identifier);
         let removed = self
            .packs
            .remove(&identifier)
            .expect("missing evicted pack");
         self.total -= removed.size;
         info!("evicted pack {identifier}, {} bytes", removed.size);
      }
      Ok(())
   }

   pub fn stats(&self) -> Stats {
      Stats {
         keys:  self.index.len(),
         packs: self.packs.len(),
         bytes: self.total,
         live:  self.packs.values().map(|pack| pack.live).sum(),
      }
   }

   pub fn close(&self) -> io::Result<()> {
      self.seal(self.active)
   }
}

fn check_lengths(key_length: u32, length: u32) -> io::Result<()> {
   if key_length == 0 || key_length > 4096 || length as usize > MAX_VALUE_SIZE {
      return Err(io::Error::new(
         io::ErrorKind::InvalidData,
         "invalid cache record lengths",
      ));
   }
   Ok(())
}

fn check_key(key: &str) -> io::Result<()> {
   if key.chars().any(char::is_whitespace) {
      return Err(io::Error::new(
         io::ErrorKind::InvalidData,
         "invalid cache key",
      ));
   }
   Ok(())
}

fn read_key(reader: &mut impl Read, length: u32) -> io::Result<String> {
   let mut bytes = vec![0; length as usize];
   reader.read_exact(&mut bytes)?;
   let key = String::from_utf8(bytes).map_err(io::Error::other)?;
   check_key(&key)?;
   Ok(key)
}

fn read_header<const SIZE: usize>(reader: &mut impl Read) -> io::Result<Option<[u8; SIZE]>> {
   let mut header = [0; SIZE];
   if reader.read(&mut header[..1])? == 0 {
      return Ok(None);
   }
   reader.read_exact(&mut header[1..])?;
   Ok(Some(header))
}

#[cfg(test)]
mod tests {
   use std::{
      fs::{
         OpenOptions,
         metadata,
      },
      io::Write,
      os::unix::fs::FileExt,
   };

   use crate::{
      store::{
         HEADER_SIZE,
         PACK_LIMIT,
         Store,
      },
      test_directory::TestDirectory,
   };

   fn read(store: &Store, key: &str) -> Option<Vec<u8>> {
      let object = store.get(key)?;
      let mut bytes = vec![0; object.length as usize];
      object
         .file
         .read_exact_at(&mut bytes, object.offset)
         .expect("read cache value");
      Some(bytes)
   }

   #[test]
   fn put_get_reopen() {
      let directory = TestDirectory::new();
      let mut initial = Store::open(&directory, 0).expect("open store");
      initial.put("o/aa", b"one").expect("first value");
      initial.put("o/bb", b"two").expect("second value");
      initial.put("o/aa", b"three").expect("replace value");
      assert_eq!(read(&initial, "o/aa").as_deref(), Some(b"three".as_slice()));
      assert_eq!(read(&initial, "o/zz"), None);
      drop(initial);
      let scanned = Store::open(&directory, 0).expect("scan unsealed pack");
      assert_eq!(read(&scanned, "o/aa").as_deref(), Some(b"three".as_slice()));
      scanned.close().expect("seal store");
      drop(scanned);
      let sealed = Store::open(&directory, 0).expect("load hints");
      assert_eq!(read(&sealed, "o/bb").as_deref(), Some(b"two".as_slice()));
   }

   #[test]
   fn torn_tail() {
      let directory = TestDirectory::new();
      let mut initial = Store::open(&directory, 0).expect("open store");
      initial.put("key", b"value").expect("write value");
      let path = initial.pack_path(initial.active, "pack");
      OpenOptions::new()
         .append(true)
         .open(&path)
         .expect("open pack")
         .write_all(&[9, 0, 0, 0, 200, 0, 0, 0, b'x'])
         .expect("append torn record");
      drop(initial);
      let repaired = Store::open(&directory, 0).expect("recover torn pack");
      assert_eq!(read(&repaired, "key").as_deref(), Some(b"value".as_slice()));
      assert_eq!(
         metadata(path).expect("stat pack").len(),
         HEADER_SIZE + 3 + 5
      );
   }

   #[test]
   fn evict_keeps_read_pack() {
      let directory = TestDirectory::new();
      let mut store = Store::open(&directory, 2 * PACK_LIMIT + PACK_LIMIT / 2).expect("open store");
      let value = vec![1; 1 << 20];
      let per_pack = PACK_LIMIT as usize / value.len();
      for number in 0..2 * per_pack {
         store
            .put(&format!("o/{number}"), &value)
            .expect("fill packs");
      }
      assert!(read(&store, "o/0").is_some(), "first pack is available");
      for number in 2 * per_pack..7 * per_pack / 2 {
         store
            .put(&format!("o/{number}"), &value)
            .expect("trigger eviction");
      }
      assert!(read(&store, "o/0").is_some(), "recently read pack survives");
      assert!(
         read(&store, &format!("o/{}", per_pack + 1)).is_none(),
         "unread pack is evicted"
      );
   }

   #[test]
   fn evict_oldest_pack() {
      let directory = TestDirectory::new();
      let mut store = Store::open(&directory, PACK_LIMIT + PACK_LIMIT / 2).expect("open store");
      let value = vec![1; 1 << 20];
      let count = PACK_LIMIT as usize * 5 / 2 / value.len();
      for number in 0..count {
         store
            .put(&format!("o/{number}"), &value)
            .expect("fill packs");
      }
      assert!(read(&store, "o/0").is_none(), "oldest value is evicted");
      assert!(
         read(&store, &format!("o/{}", count - 1)).is_some(),
         "newest value survives"
      );
      assert!(
         !directory.join("000001.pack").exists(),
         "evicted pack is removed"
      );
   }
}
