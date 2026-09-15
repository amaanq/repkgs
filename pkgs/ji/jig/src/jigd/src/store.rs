use std::{
   collections::BTreeMap,
   fmt::{
      Display,
      Formatter,
      Result as FormatResult,
   },
   fs::{
      File,
      OpenOptions,
      copy,
      create_dir_all,
      hard_link,
      read_dir,
      remove_file,
      rename,
   },
   io::{
      self,
      BufReader,
      BufWriter,
      Cursor,
      Read,
      Seek as _,
      Write,
   },
   os::unix::{
      fs::FileExt as _,
      net::UnixStream,
   },
   path::{
      Path,
      PathBuf,
   },
   sync::{
      Arc,
      Condvar,
      Mutex,
      RwLock,
      RwLockReadGuard,
      RwLockWriteGuard,
      atomic::{
         AtomicU64,
         Ordering,
      },
   },
   thread::{
      Builder,
      JoinHandle,
   },
   time::{
      Duration,
      SystemTime,
      UNIX_EPOCH,
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
use tempfile::tempfile_in;
use wire::{
   MAX_KEY,
   MAX_OBJECT,
   check_key,
};

use crate::{
   note,
   warn,
};

const PACK_LIMIT: u64 = 256 << 20;
const HEADER_SIZE: u64 = 8;
const SPOOL_MIN: u64 = 4 << 20;
const TOUCH_EVERY: Duration = Duration::from_secs(60);

pub struct Tier {
   pub directory: PathBuf,
   pub budget:    u64,
}

struct Location {
   pack:   u64,
   offset: u64,
   length: u32,
}

struct Pack {
   file:    Arc<File>,
   tier:    usize,
   size:    u64,
   live:    u64,
   used:    AtomicU64,
   touched: AtomicU64,
}

impl Pack {
   const fn stale(&self) -> bool {
      self.live * 2 < self.size
   }
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
      let mut remaining = usize::try_from(self.length).map_err(io::Error::other)?;
      while remaining > 0 {
         match sendfile(
            stream,
            &*self.file,
            Some(&mut offset),
            remaining.min(1 << 20_i32),
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
   fn fmt(&self, f: &mut Formatter<'_>) -> FormatResult {
      write!(
         f,
         "keys={} packs={} bytes={} live={}",
         self.keys, self.packs, self.bytes, self.live
      )
   }
}

#[derive(Default)]
struct State {
   index:  BTreeMap<String, Location>,
   packs:  BTreeMap<u64, Pack>,
   active: u64,
   totals: Vec<u64>,
   closed: bool,
}

impl State {
   #[expect(
      clippy::expect_used,
      reason = "indexed locations must refer to loaded packs"
   )]
   fn insert(&mut self, key: String, location: Location) {
      if let Some(previous) = self.index.get(&key)
         && previous.pack > location.pack
      {
         return;
      }
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
}

#[derive(Default)]
struct Demotion {
   pending: bool,
   stopped: bool,
}

struct Shared {
   tiers:    Vec<Tier>,
   _locks:   Vec<File>,
   state:    RwLock<State>,
   demotion: Mutex<Demotion>,
   changed:  Condvar,
}

pub struct Store {
   shared:  Arc<Shared>,
   demoter: Mutex<Option<JoinHandle<()>>>,
}

fn nanos(time: SystemTime) -> u64 {
   time.duration_since(UNIX_EPOCH).map_or(0, |since| {
      u64::try_from(since.as_nanos()).unwrap_or(u64::MAX)
   })
}

fn now() -> u64 {
   nanos(SystemTime::now())
}

impl Shared {
   fn pack_path(&self, tier: usize, identifier: u64, extension: &str) -> PathBuf {
      self.tiers[tier]
         .directory
         .join(format!("{identifier:06}.{extension}"))
   }

   #[expect(
      clippy::expect_used,
      reason = "poisoned store state violates its invariants"
   )]
   fn read(&self) -> RwLockReadGuard<'_, State> {
      self.state.read().expect("store lock poisoned")
   }

   #[expect(
      clippy::expect_used,
      reason = "poisoned store state violates its invariants"
   )]
   fn write(&self) -> RwLockWriteGuard<'_, State> {
      self.state.write().expect("store lock poisoned")
   }

   #[expect(
      clippy::too_many_lines,
      reason = "pack loading handles hint parsing and fallback scan in one place"
   )]
   #[expect(
      clippy::expect_used,
      reason = "the pack was inserted before its size is updated"
   )]
   fn load(&self, state: &mut State, tier: usize, identifier: u64) -> io::Result<()> {
      let file = Arc::new(
         OpenOptions::new()
            .read(true)
            .write(true)
            .open(self.pack_path(tier, identifier, "pack"))?,
      );
      let metadata = file.metadata()?;
      let size = metadata.len();
      let modified = nanos(metadata.modified()?);
      let pack = Pack {
         file: Arc::clone(&file),
         tier,
         size,
         live: 0,
         used: AtomicU64::new(modified),
         touched: AtomicU64::new(modified),
      };
      match File::open(self.pack_path(tier, identifier, "hint")) {
         Ok(hint) => {
            let mut reader = BufReader::new(hint);
            let mut records = Vec::new();
            while let Some(header) = read_header::<16>(&mut reader)? {
               let key_length = u32::from_le_bytes(
                  header[..4]
                     .try_into()
                     .map_err(|_error| io::Error::other("key length slice"))?,
               );
               let length = u32::from_le_bytes(
                  header[4..8]
                     .try_into()
                     .map_err(|_error| io::Error::other("value length slice"))?,
               );
               let record_offset = u64::from_le_bytes(
                  header[8..]
                     .try_into()
                     .map_err(|_error| io::Error::other("offset slice"))?,
               );
               let key = read_key(&mut reader, key_length)?;
               let Some(offset) = record_offset.checked_add(HEADER_SIZE + u64::from(key_length))
               else {
                  continue;
               };
               if offset
                  .checked_add(u64::from(length))
                  .is_none_or(|end| end > size)
               {
                  continue;
               }
               records.push((key, offset, length));
            }
            state.packs.insert(identifier, pack);
            for (key, offset, length) in records {
               state.insert(key, Location {
                  pack: identifier,
                  offset,
                  length,
               });
            }
            state.totals[tier] += size;
         },
         Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let mut reader = BufReader::new(&*file);
            let mut records = Vec::new();
            let mut offset = 0;
            while offset + HEADER_SIZE <= size {
               let mut header = [0; 8];
               reader.read_exact(&mut header)?;
               let key_length = u32::from_le_bytes(
                  header[..4]
                     .try_into()
                     .map_err(|_error| io::Error::other("key length slice"))?,
               );
               let length = u32::from_le_bytes(
                  header[4..]
                     .try_into()
                     .map_err(|_error| io::Error::other("value length slice"))?,
               );
               let end = offset + HEADER_SIZE + u64::from(key_length) + u64::from(length);
               if check_lengths(key_length, length).is_err() || end > size {
                  break;
               }
               let Ok(key) = read_key(&mut reader, key_length) else {
                  break;
               };
               reader.seek_relative(i64::from(length))?;
               records.push((key, offset + HEADER_SIZE + u64::from(key_length), length));
               offset = end;
            }
            state.packs.insert(identifier, pack);
            for (key, value_offset, length) in records {
               state.insert(key, Location {
                  pack: identifier,
                  offset: value_offset,
                  length,
               });
            }
            if offset != size {
               warn(format_args!(
                  "truncating unfinished pack {identifier} from {size} to {offset} bytes"
               ));
               file.set_len(offset)?;
               state
                  .packs
                  .get_mut(&identifier)
                  .expect("missing loaded pack")
                  .size = offset;
            }
            state.totals[tier] += offset;
            self.seal(state, identifier)?;
         },
         Err(error) => return Err(error),
      }
      Ok(())
   }

   fn seal(&self, state: &State, identifier: u64) -> io::Result<()> {
      let tier = state.packs[&identifier].tier;
      let temporary = self.pack_path(tier, identifier, "hint.tmp");
      let mut writer = BufWriter::new(File::create(&temporary)?);
      for (key, location) in &state.index {
         if location.pack != identifier {
            continue;
         }
         let key_length = u32::try_from(key.len()).map_err(io::Error::other)?;
         let record_offset = location.offset - HEADER_SIZE - u64::from(key_length);
         writer.write_all(&key_length.to_le_bytes())?;
         writer.write_all(&location.length.to_le_bytes())?;
         writer.write_all(&record_offset.to_le_bytes())?;
         writer.write_all(key.as_bytes())?;
      }
      writer.flush()?;
      rename(temporary, self.pack_path(tier, identifier, "hint"))
   }

   fn rotate(&self, state: &mut State) -> io::Result<()> {
      if state.packs.contains_key(&state.active) {
         self.seal(state, state.active)?;
      }
      let identifier = state
         .active
         .checked_add(1)
         .ok_or_else(|| io::Error::other("pack IDs exhausted"))?;
      let file = OpenOptions::new()
         .read(true)
         .write(true)
         .create_new(true)
         .open(self.pack_path(0, identifier, "pack"))?;
      let started = now();
      state.packs.insert(identifier, Pack {
         file:    Arc::new(file),
         tier:    0,
         size:    0,
         live:    0,
         used:    AtomicU64::new(started),
         touched: AtomicU64::new(started),
      });
      state.active = identifier;
      Ok(())
   }

   fn kick(&self) {
      if let Ok(mut demotion) = self.demotion.lock() {
         demotion.pending = true;
         self.changed.notify_all();
      }
   }

   fn victim(&self, state: &State) -> Option<u64> {
      for (tier, budget) in self.tiers.iter().enumerate() {
         if budget.budget == 0 || state.totals[tier] <= budget.budget {
            continue;
         }
         let chosen = state
            .packs
            .iter()
            .filter(|&(&identifier, pack)| pack.tier == tier && identifier != state.active)
            .min_by_key(|&(&identifier, pack)| {
               (!pack.stale(), pack.used.load(Ordering::Relaxed), identifier)
            })
            .map(|(&identifier, _)| identifier);
         if chosen.is_some() {
            return chosen;
         }
      }
      None
   }

   #[expect(
      clippy::expect_used,
      reason = "selected packs must remain in the index"
   )]
   fn move_down(&self, identifier: u64) -> io::Result<()> {
      let (from, used) = {
         let state = self.read();
         let pack = &state.packs[&identifier];
         let from = pack.tier;
         let used = pack.used.load(Ordering::Relaxed);
         drop(state);
         (from, used)
      };
      if from + 1 == self.tiers.len() {
         let mut state = self.write();
         state
            .index
            .retain(|_, location| location.pack != identifier);
         let removed = state
            .packs
            .remove(&identifier)
            .expect("missing evicted pack");
         remove_file(self.pack_path(from, identifier, "pack"))?;
         drop(remove_file(self.pack_path(from, identifier, "hint")));
         state.totals[from] -= removed.size;
         let size = removed.size;
         let live = removed.live;
         drop(state);
         note(format_args!(
            "evicted pack {identifier:06}, {} MiB, {} MiB live",
            size >> 20_u32,
            live >> 20_u32
         ));
         return Ok(());
      }
      for extension in ["pack", "hint"] {
         move_file(
            &self.pack_path(from, identifier, extension),
            &self.pack_path(from + 1, identifier, extension),
         )?;
      }
      let file = OpenOptions::new()
         .read(true)
         .write(true)
         .open(self.pack_path(from + 1, identifier, "pack"))?;
      file.set_modified(UNIX_EPOCH + Duration::from_nanos(used))?;
      let mut state = self.write();
      let pack = state
         .packs
         .get_mut(&identifier)
         .expect("missing demoted pack");
      pack.file = Arc::new(file);
      pack.tier = from + 1;
      let (size, live) = (pack.size, pack.live);
      state.totals[from] -= size;
      state.totals[from + 1] += size;
      drop(state);
      remove_file(self.pack_path(from, identifier, "pack"))?;
      drop(remove_file(self.pack_path(from, identifier, "hint")));
      note(format_args!(
         "demoted pack {identifier:06} to {}, {} MiB, {} MiB live",
         self.tiers[from + 1].directory.display(),
         size >> 20_u32,
         live >> 20_u32
      ));
      Ok(())
   }

   fn demote(&self) {
      loop {
         {
            let Ok(mut demotion) = self.demotion.lock() else {
               return;
            };
            while !demotion.pending && !demotion.stopped {
               let Ok(next_demotion) = self.changed.wait(demotion) else {
                  return;
               };
               demotion = next_demotion;
            }
            if demotion.stopped {
               return;
            }
            demotion.pending = false;
         };
         loop {
            let victim = self.victim(&self.read());
            let Some(identifier) = victim else {
               break;
            };
            if let Err(error) = self.move_down(identifier) {
               warn(format_args!("pack {identifier:06}: {error}"));
               break;
            }
         }
      }
   }
}

struct Positioned<'file> {
   file:   &'file File,
   offset: u64,
}

impl Write for Positioned<'_> {
   fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
      let written = self.file.write_at(buf, self.offset)?;
      self.offset += u64::try_from(written).map_err(io::Error::other)?;
      Ok(written)
   }

   fn flush(&mut self) -> io::Result<()> {
      Ok(())
   }
}

impl Store {
   pub fn open(tiers: Vec<Tier>) -> io::Result<Self> {
      let mut locks = Vec::new();
      for tier in &tiers {
         create_dir_all(&tier.directory)?;
         let lock = File::open(&tier.directory)?;
         flock(&lock, FlockOperation::NonBlockingLockExclusive)?;
         locks.push(lock);
      }
      let shared = Arc::new(Shared {
         state: RwLock::new(State {
            totals: vec![0; tiers.len()],
            ..State::default()
         }),
         tiers,
         _locks: locks,
         demotion: Mutex::new(Demotion::default()),
         changed: Condvar::new(),
      });
      {
         let mut state = shared.write();
         let mut last = 0;
         for tier in 0..shared.tiers.len() {
            let mut identifiers = Vec::new();
            for entry in read_dir(&shared.tiers[tier].directory)? {
               let path = entry?.path();
               if path
                  .extension()
                  .is_some_and(|extension| extension == "pack")
               {
                  let Some(identifier) = path
                     .file_stem()
                     .and_then(|stem| stem.to_str())
                     .and_then(|stem| stem.parse::<u64>().ok())
                     .filter(|identifier| {
                        path.file_name() == shared.pack_path(tier, *identifier, "pack").file_name()
                     })
                  else {
                     warn(format_args!(
                        "ignoring pack file {} with a noncanonical name",
                        path.display()
                     ));
                     continue;
                  };
                  identifiers.push(identifier);
               }
            }
            identifiers.sort_unstable();
            for identifier in identifiers {
               if state.packs.contains_key(&identifier) {
                  drop(remove_file(shared.pack_path(tier, identifier, "pack")));
                  drop(remove_file(shared.pack_path(tier, identifier, "hint")));
                  continue;
               }
               if let Err(error) = shared.load(&mut state, tier, identifier) {
                  warn(format_args!("pack {identifier:06}: {error} (skipped)"));
                  continue;
               }
               last = last.max(identifier);
            }
         }
         state.active = last;
         shared.rotate(&mut state)?;
         drop(state);
      };
      let worker = Arc::clone(&shared);
      let demoter = Builder::new()
         .name("demoter".to_owned())
         .spawn(move || worker.demote())?;
      let store = Self {
         shared,
         demoter: Mutex::new(Some(demoter)),
      };
      store.shared.kick();
      Ok(store)
   }

   pub fn get(&self, key: &str) -> Option<Value> {
      let (file, offset, length) = {
         let state = self.shared.read();
         let location = state.index.get(key)?;
         let pack = &state.packs[&location.pack];
         let current = now();
         let file = Arc::clone(&pack.file);
         let offset = location.offset;
         let length = location.length;
         let touched = pack.touched.load(Ordering::Relaxed);
         pack.used.store(current, Ordering::Relaxed);
         let touch_interval = u64::try_from(TOUCH_EVERY.as_nanos()).unwrap_or(u64::MAX);
         let need_touch = current.saturating_sub(touched) > touch_interval
            && pack
               .touched
               .compare_exchange(touched, current, Ordering::Relaxed, Ordering::Relaxed)
               .is_ok();
         drop(state);
         if need_touch {
            drop(file.set_modified(UNIX_EPOCH + Duration::from_nanos(current)));
         }
         (file, offset, length)
      };
      Some(Value {
         file,
         offset,
         length,
      })
   }

   #[expect(
      clippy::expect_used,
      reason = "the active pack must exist after rotation"
   )]
   pub fn put<Reader>(&self, key: &str, body: &mut Reader, length: usize) -> io::Result<()>
   where
      Reader: Read,
   {
      let key_length = u32::try_from(key.len()).map_err(io::Error::other)?;
      let value_length = u32::try_from(length).map_err(io::Error::other)?;
      check_lengths(key_length, value_length)?;
      check_key(key)?;
      let size = u64::from(value_length);
      let record_size = HEADER_SIZE + u64::from(key_length) + size;
      let staged: Box<dyn Read> = if size < SPOOL_MIN {
         let mut buffer = Vec::new();
         buffer.try_reserve_exact(length).map_err(io::Error::other)?;
         buffer.resize(length, 0);
         body.read_exact(&mut buffer)?;
         Box::new(Cursor::new(buffer))
      } else {
         let mut spool = tempfile_in(&self.shared.tiers[0].directory)?;
         let copied = io::copy(&mut body.take(size), &mut spool)?;
         if copied != size {
            return Err(io::Error::new(
               io::ErrorKind::UnexpectedEof,
               "short cache object body",
            ));
         }
         spool.rewind()?;
         Box::new(spool)
      };
      let mut state = self.shared.write();
      if state.closed {
         return Err(io::Error::other("cache store is closed"));
      }
      let active_size = state.packs[&state.active].size;
      if active_size > 0 && active_size + record_size > PACK_LIMIT {
         self.shared.rotate(&mut state)?;
      }
      let active = state.active;
      let pack = state.packs.get_mut(&active).expect("missing active pack");
      let mut header = Vec::with_capacity(8 + key.len());
      header.extend_from_slice(&key_length.to_le_bytes());
      header.extend_from_slice(&value_length.to_le_bytes());
      header.extend_from_slice(key.as_bytes());
      let header_length = u64::try_from(header.len()).map_err(io::Error::other)?;
      let offset = pack.size + header_length;
      let start = pack.size;
      let written = (|| {
         pack.file.write_all_at(&header, start)?;
         let mut target = Positioned {
            file: &pack.file,
            offset,
         };
         if io::copy(&mut staged.take(size), &mut target)? != size {
            return Err(io::Error::new(
               io::ErrorKind::UnexpectedEof,
               "short cache object body",
            ));
         }
         Ok(())
      })();
      if let Err(error) = written {
         pack.file.set_len(start)?;
         return Err(error);
      }
      pack.size += record_size;
      state.totals[0] += record_size;
      state.insert(key.to_owned(), Location {
         pack: active,
         offset,
         length: value_length,
      });
      let budget = self.shared.tiers[0].budget;
      let needs_kick = budget > 0 && state.totals[0] > budget;
      drop(state);
      if needs_kick {
         self.shared.kick();
      }
      Ok(())
   }

   pub fn stats(&self) -> Stats {
      let state = self.shared.read();
      Stats {
         keys:  state.index.len(),
         packs: state.packs.len(),
         bytes: state.totals.iter().sum(),
         live:  state.packs.values().map(|pack| pack.live).sum(),
      }
   }

   fn stop(&self) {
      if let Ok(mut demotion) = self.shared.demotion.lock() {
         demotion.stopped = true;
         self.shared.changed.notify_all();
      }
      if let Ok(mut demoter_guard) = self.demoter.lock()
         && let Some(worker) = demoter_guard.take()
      {
         drop(worker.join());
      }
   }

   pub fn close(&self) -> io::Result<()> {
      self.stop();
      let mut state = self.shared.write();
      state.closed = true;
      self.shared.seal(&state, state.active)
   }

   #[cfg(test)]
   fn wait(&self) {
      use std::thread;
      while self.shared.victim(&self.shared.read()).is_some() {
         thread::sleep(Duration::from_millis(1));
      }
   }
}

impl Drop for Store {
   fn drop(&mut self) {
      self.stop();
   }
}

fn move_file(from: &Path, to: &Path) -> io::Result<()> {
   match hard_link(from, to) {
      Ok(()) => return Ok(()),
      Err(error) if error.kind() == io::ErrorKind::AlreadyExists => return Ok(()),
      Err(_) => {},
   }
   let temporary = to.with_extension("tmp");
   copy(from, &temporary)?;
   rename(temporary, to)
}

fn check_lengths(key_length: u32, length: u32) -> io::Result<()> {
   let key_len = usize::try_from(key_length).map_err(io::Error::other)?;
   let val_len = usize::try_from(length).map_err(io::Error::other)?;
   if key_length == 0 || key_len > MAX_KEY || val_len > MAX_OBJECT {
      return Err(io::Error::new(
         io::ErrorKind::InvalidData,
         "invalid cache record lengths",
      ));
   }
   Ok(())
}

fn read_key(reader: &mut impl Read, length: u32) -> io::Result<String> {
   let byte_count = usize::try_from(length).map_err(io::Error::other)?;
   let mut bytes = vec![0; byte_count];
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
#[expect(
   clippy::as_conversions,
   clippy::cast_possible_truncation,
   clippy::expect_used,
   reason = "tests"
)]
mod tests {
   use std::{
      fs::{
         File,
         OpenOptions,
         metadata,
      },
      io::Write as _,
      ops::Range,
      os::unix::fs::FileExt as _,
      path::Path,
      sync::atomic::Ordering,
      time::{
         Duration,
         SystemTime,
      },
   };

   use crate::{
      store::{
         HEADER_SIZE,
         PACK_LIMIT,
         Store,
         Tier,
      },
      test_directory::TestDirectory,
   };

   fn open(directory: &Path, budget: u64) -> Store {
      Store::open(vec![Tier {
         directory: directory.to_path_buf(),
         budget,
      }])
      .expect("open store")
   }

   fn read(store: &Store, key: &str) -> Option<Vec<u8>> {
      let object = store.get(key)?;
      let mut bytes = vec![0; object.length as usize];
      object
         .file
         .read_exact_at(&mut bytes, object.offset)
         .expect("read cache value");
      Some(bytes)
   }

   fn fill(store: &Store, numbers: Range<usize>) {
      let value = vec![1; 1 << 20];
      for number in numbers {
         put(store, &format!("o/{number}"), &value);
      }
      store.wait();
   }

   fn put(store: &Store, key: &str, value: &[u8]) {
      let length = value.len();
      store.put(key, &mut &*value, length).expect("write value");
   }

   const PER_PACK: usize = PACK_LIMIT as usize / (1 << 20);

   #[test]
   fn put_get_reopen() {
      let directory = TestDirectory::new();
      let initial = open(&directory, 0);
      put(&initial, "o/aa", b"one");
      put(&initial, "o/bb", b"two");
      put(&initial, "o/aa", b"three");
      assert_eq!(read(&initial, "o/aa").as_deref(), Some(b"three".as_slice()));
      assert_eq!(read(&initial, "o/zz"), None);
      drop(initial);
      let scanned = open(&directory, 0);
      assert_eq!(read(&scanned, "o/aa").as_deref(), Some(b"three".as_slice()));
      scanned.close().expect("seal store");
      drop(scanned);
      let sealed = open(&directory, 0);
      assert_eq!(read(&sealed, "o/bb").as_deref(), Some(b"two".as_slice()));
   }

   #[test]
   fn torn_tail() {
      let directory = TestDirectory::new();
      let initial = open(&directory, 0);
      put(&initial, "key", b"value");
      let path = directory.join("000001.pack");
      OpenOptions::new()
         .append(true)
         .open(&path)
         .expect("open pack")
         .write_all(&[9, 0, 0, 0, 200, 0, 0, 0, b'x'])
         .expect("append torn record");
      drop(initial);
      let repaired = open(&directory, 0);
      assert_eq!(read(&repaired, "key").as_deref(), Some(b"value".as_slice()));
      assert_eq!(
         metadata(path).expect("stat pack").len(),
         HEADER_SIZE + 3 + 5
      );
   }

   #[test]
   fn evict_keeps_read_pack() {
      let directory = TestDirectory::new();
      let store = open(&directory, 2 * PACK_LIMIT + PACK_LIMIT / 2);
      fill(&store, 0..2 * PER_PACK);
      assert!(read(&store, "o/0").is_some(), "first pack is available");
      fill(&store, 2 * PER_PACK..7 * PER_PACK / 2);
      assert!(read(&store, "o/0").is_some(), "recently read pack survives");
      assert!(
         read(&store, &format!("o/{}", PER_PACK + 1)).is_none(),
         "unread pack is evicted"
      );
   }

   #[test]
   fn evict_recency_survives_restart() {
      let directory = TestDirectory::new();
      let store = open(&directory, 0);
      fill(&store, 0..2 * PER_PACK);
      store.close().expect("seal store");
      drop(store);
      let later = SystemTime::now() + Duration::from_secs(3600);
      File::open(directory.join("000001.pack"))
         .expect("open pack")
         .set_modified(later)
         .expect("touch pack");
      let reopened = open(&directory, 2 * PACK_LIMIT + PACK_LIMIT / 2);
      {
         let state = reopened.shared.read();
         assert!(
            state.packs[&1].used.load(Ordering::Relaxed)
               > state.packs[&2].used.load(Ordering::Relaxed),
            "pack 1 is newer than pack 2 after the restart"
         );
         drop(state);
      };
      fill(&reopened, 2 * PER_PACK..7 * PER_PACK / 2);
      assert!(
         read(&reopened, "o/0").is_some(),
         "pack read before the restart survives"
      );
      assert!(
         read(&reopened, &format!("o/{}", PER_PACK + 1)).is_none(),
         "never read pack is evicted"
      );
      let stale = SystemTime::now() - Duration::from_secs(600);
      File::open(directory.join("000001.pack"))
         .expect("open pack")
         .set_modified(stale)
         .expect("age pack");
      reopened.close().expect("seal store");
      drop(reopened);
      let touched = open(&directory, 0);
      read(&touched, "o/0").expect("read aged pack");
      assert!(
         metadata(directory.join("000001.pack"))
            .expect("stat pack")
            .modified()
            .expect("mtime")
            > stale,
         "a read moves the pack's mtime"
      );
   }

   #[test]
   fn evict_superseded_first() {
      let directory = TestDirectory::new();
      let store = open(&directory, 2 * PACK_LIMIT + PACK_LIMIT / 2);
      fill(&store, 0..PER_PACK);
      fill(&store, 0..PER_PACK);
      read(&store, "o/0").expect("served from pack 2");
      store.shared.read().packs[&1].used.store(
         super::now() + Duration::from_secs(3600).as_nanos() as u64,
         Ordering::Relaxed,
      );
      fill(&store, PER_PACK..5 * PER_PACK / 2);
      assert!(
         !directory.join("000001.pack").exists(),
         "superseded pack 1 is gone"
      );
      assert!(read(&store, "o/0").is_some(), "live pack 2 survives");
   }

   #[test]
   fn tiers() {
      let hot = TestDirectory::new();
      let cold = TestDirectory::new();
      let tiers = || {
         vec![
            Tier {
               directory: hot.to_path_buf(),
               budget:    2 * PACK_LIMIT,
            },
            Tier {
               directory: cold.to_path_buf(),
               budget:    PACK_LIMIT + PACK_LIMIT / 2,
            },
         ]
      };
      let store = Store::open(tiers()).expect("open tiers");
      fill(&store, 0..5 * PER_PACK / 2);
      assert!(cold.join("000001.pack").exists(), "pack 1 is demoted");
      assert!(
         !hot.join("000001.pack").exists(),
         "pack 1 left the hot tier"
      );
      assert!(read(&store, "o/0").is_some(), "served from the cold tier");
      fill(&store, 5 * PER_PACK / 2..7 * PER_PACK / 2);
      assert!(
         read(&store, &format!("o/{}", PER_PACK + 1)).is_none(),
         "pack 2 fell out of the cold budget"
      );
      assert!(read(&store, "o/0").is_some(), "read pack 1 outlives pack 2");
      store.close().expect("seal store");
      drop(store);
      let reopened = Store::open(
         tiers()
            .into_iter()
            .map(|tier| Tier { budget: 0, ..tier })
            .collect(),
      )
      .expect("reopen tiers");
      assert!(
         read(&reopened, "o/0").is_some(),
         "cold pack found after restart"
      );
      assert_eq!(reopened.shared.read().packs[&1].tier, 1);
   }

   #[test]
   fn evict_oldest_pack() {
      let directory = TestDirectory::new();
      let store = open(&directory, PACK_LIMIT + PACK_LIMIT / 2);
      let count = PACK_LIMIT as usize * 5 / 2 / (1 << 20_i32);
      fill(&store, 0..count);
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
